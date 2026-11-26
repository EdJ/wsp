//! The hub's wire, held to today's byte for byte: every frame this crate
//! writes is written through `wsp-api` too (built field by field by `api_*`
//! below, never through JSON), and the two lines are the same bytes; each
//! side decodes the other's line to the value it started from.
//!
//! The tables are table-driven as `wsp-api`'s `tests/round_trip.rs` is: each
//! table's coverage is held by an exhaustive match (the `*_ordinal`s), so a
//! variant added to this crate does not compile until it has an arm and a
//! frame. Optional fields are written both present and absent.
//!
//! Then: the verb tables are the variants and a subset of today's; the state
//! core's verbs a hub does not serve decode as `Call::Unknown` keeping their
//! id; older peers' frames decode; the text form of a principal; the pinned
//! spelling.

use std::collections::{BTreeMap, BTreeSet};

use serde_json::{json, Value};
use wsp_api as api;
use wsp_hub_protocol as p;
use wsp_hub_protocol::{
    Answer, ApiToken, Blob, Call, Code, Failure, Frame, Hello, HubMessage, IdentityCommand, IdsAlloc, LeaseHold,
    LeaseTake, Login, LoginDone, LoginPrompt, Move, MsgSend, NoArgs, Outcome, RefMove, Refused, Reply, ReqId,
    Request, SyncCall, Verb, Welcome,
};
use wsp_objects::{ObjectId, Refs, WorkspaceId};
use wsp_store_contract::{
    At, Credential, Event, EventId, EventKind, Feed, IdempotencyKey, IdentityState, Lease, LeaseName, MachineName,
    Principal, Refusal, Rev, RowId, SeatId, SharedName, SharedWorkspace, State, Subject, Subscribe, Token, TokenId,
    User, UserName,
};

// ---------------------------------------------------------------- the map to today's types

fn api_token(t: &ApiToken) -> api::ApiToken {
    api::ApiToken(t.0.clone())
}

fn api_frame(f: &Frame) -> Option<api::Frame> {
    Some(match f {
        Frame::Hello(h) => api::Frame::Hello(api::Hello {
            token: api_token(&h.token),
            by: h.by.clone(),
            client: h.client.clone(),
            protocol: h.protocol,
        }),
        Frame::Welcome(w) => api::Frame::Welcome(api::Welcome {
            server: w.server.clone(),
            protocol: w.protocol,
            head: w.head,
            by: w.by.clone(),
        }),
        Frame::Refused(r) => api::Frame::Refused(api::Refused { code: api_code(r.code), text: r.text.clone() }),
        Frame::Request(r) => api::Frame::Request(api::Request {
            v: r.v,
            id: api::ReqId(r.id.0),
            key: r.key.clone(),
            by: r.by.clone(),
            call: api_call(&r.call),
        }),
        Frame::Reply(r) => api::Frame::Reply(api::Reply {
            id: api::ReqId(r.id.0),
            outcome: match &r.outcome {
                Outcome::Ok(a) => api::Outcome::Ok(api_answer(a)),
                Outcome::Err(e) => api::Outcome::Err(api_failure(e)),
            },
            events: r.events.clone(),
        }),
        Frame::Feed(feed) => api::Frame::Feed(feed.clone()),
        // This repo's `wsp-api` has no login frames (the hub repo's copy does):
        // they are held to pinned bytes instead, in `login_line`.
        Frame::Login(_) | Frame::LoginPrompt(_) | Frame::LoginDone(_) => return None,
    })
}

fn api_call(c: &Call) -> api::Call {
    match c {
        Call::Verb(v) => api::Call::Verb(match v {
            Verb::State(NoArgs {}) => api::Verb::State(api::NoArgs {}),
            Verb::Events { from, limit } => api::Verb::Events { from: *from, limit: *limit },
            Verb::Subscribe(s) => api::Verb::Subscribe(s.clone()),
            Verb::Whoami(NoArgs {}) => api::Verb::Whoami(api::NoArgs {}),
        }),
        Call::Command(c) => api::Call::Command(api_command(c)),
        Call::Sync(s) => api::Call::Sync(api_sync(s)),
        Call::Unknown { verb, args } => api::Call::Unknown { verb: verb.clone(), args: args.clone() },
    }
}

fn api_command(c: &IdentityCommand) -> wsp_store_contract::Command {
    use wsp_store_contract::Command as C;
    match c.clone() {
        IdentityCommand::HubUserAdd { name, admin } => C::HubUserAdd { name, admin },
        IdentityCommand::HubUserRevoke { name } => C::HubUserRevoke { name },
        IdentityCommand::HubTokenIssue { id, user, hash } => C::HubTokenIssue { id, user, hash },
        IdentityCommand::HubTokenRevoke { id } => C::HubTokenRevoke { id },
        IdentityCommand::HubWorkspaceAdd { name } => C::HubWorkspaceAdd { name },
        IdentityCommand::HubMemberSet { workspace, user, member } => C::HubMemberSet { workspace, user, member },
    }
}

fn api_blob(b: &Blob) -> api::Blob {
    api::Blob { id: b.id, bytes: b.bytes.clone() }
}

fn api_sync(s: &SyncCall) -> api::SyncCall {
    match s.clone() {
        SyncCall::Heads { workspace } => api::SyncCall::Heads { workspace },
        SyncCall::Have { ids } => api::SyncCall::Have { ids },
        SyncCall::Put { objects } => api::SyncCall::Put { objects: objects.iter().map(api_blob).collect() },
        SyncCall::Get { ids } => api::SyncCall::Get { ids },
        SyncCall::Move(RefMove { workspace, subject, from, to }) => {
            api::SyncCall::Move(api::RefMove { workspace, subject, from, to })
        }
        SyncCall::Alloc(IdsAlloc { workspace, project, n, past }) => {
            api::SyncCall::Alloc(api::IdsAlloc { workspace, project, n, past })
        }
        SyncCall::LeaseTake(LeaseTake { workspace, row }) => api::SyncCall::LeaseTake(api::LeaseTake { workspace, row }),
        SyncCall::LeaseRenew(LeaseHold { row, token }) => api::SyncCall::LeaseRenew(api::LeaseHold { row, token }),
        SyncCall::LeaseRelease(LeaseHold { row, token }) => api::SyncCall::LeaseRelease(api::LeaseHold { row, token }),
        SyncCall::MsgSend(MsgSend { to, from, seat, text, re }) => {
            api::SyncCall::MsgSend(api::MsgSend { to, from, seat, text, re })
        }
        SyncCall::MsgTake(NoArgs {}) => api::SyncCall::MsgTake(api::NoArgs {}),
        SyncCall::MsgAck { ids } => api::SyncCall::MsgAck { ids },
    }
}

fn api_message(m: &HubMessage) -> api::HubMessage {
    let HubMessage { id, to, user, from_user, from, seat, text, re, at } = m.clone();
    api::HubMessage { id, to, user, from_user, from, seat, text, re, at }
}

fn api_answer(a: &Answer) -> api::Answer {
    match a.clone() {
        Answer::State { state, at } => api::Answer::State { state, at },
        Answer::Events { events, head, through } => api::Answer::Events { events, head, through },
        Answer::Subscribed => api::Answer::Subscribed,
        Answer::Applied => api::Answer::Applied,
        Answer::Whoami { by, server } => api::Answer::Whoami { by, server },
        Answer::Heads(refs) => api::Answer::Heads(refs),
        Answer::Missing(ids) => api::Answer::Missing(ids),
        Answer::Stored { stored } => api::Answer::Stored { stored },
        Answer::Objects(blobs) => api::Answer::Objects(blobs.iter().map(api_blob).collect()),
        Answer::Move(Move::Moved { head }) => api::Answer::Move(api::Move::Moved { head }),
        Answer::Move(Move::Stale { head }) => api::Answer::Move(api::Move::Stale { head }),
        Answer::Ids(ids) => api::Answer::Ids(ids),
        Answer::Lease { token, expires } => api::Answer::Lease { token, expires },
        Answer::Released => api::Answer::Released,
        Answer::Queued { id, user } => api::Answer::Queued { id, user },
        Answer::Messages(ms) => api::Answer::Messages(ms.iter().map(api_message).collect()),
        Answer::Acked { acked } => api::Answer::Acked { acked },
    }
}

fn api_failure(f: &Failure) -> api::Failure {
    api::Failure { code: api_code(f.code), text: f.text.clone(), refusal: f.refusal.clone() }
}

fn api_code(c: Code) -> api::Code {
    match c {
        Code::Stale => api::Code::Stale,
        Code::TooOld => api::Code::TooOld,
        Code::Conflict => api::Code::Conflict,
        Code::Held => api::Code::Held,
        Code::NotFound => api::Code::NotFound,
        Code::Illegal => api::Code::Illegal,
        Code::Forbidden => api::Code::Forbidden,
        Code::Invalid => api::Code::Invalid,
        Code::Malformed => api::Code::Malformed,
        Code::Unauthorised => api::Code::Unauthorised,
        Code::Protocol => api::Code::Protocol,
        Code::Unsupported => api::Code::Unsupported,
        Code::Backend => api::Code::Backend,
        Code::Fenced => api::Code::Fenced,
        Code::TooLarge => api::Code::TooLarge,
        Code::Unknown => api::Code::Unknown,
    }
}

// ---------------------------------------------------------------- the harness

/// One frame both ways: this crate's line and today's are the same bytes, one
/// line; today's decodes this crate's to the value it maps to, and this crate
/// decodes today's to the frame it started from and writes it again the same.
/// Returns the line's JSON.
fn wire(f: &Frame) -> Value {
    let ours = serde_json::to_string(f).unwrap();
    golden(&ours);
    let Some(theirs_value) = api_frame(f) else {
        assert_eq!(ours, login_line(f), "the login frame's pinned bytes");
        let read_here: Frame = serde_json::from_str(&ours).unwrap();
        assert_eq!(&read_here, f, "this crate reads the pinned line: {ours}");
        return serde_json::from_str(&ours).unwrap();
    };
    let theirs = serde_json::to_string(&theirs_value).unwrap();
    assert_eq!(ours, theirs, "the same bytes as today's wsp-api");
    assert!(!ours.contains('\n'), "a frame is one line: {ours}");
    let read_by_today: api::Frame = serde_json::from_str(&ours).unwrap();
    assert_eq!(read_by_today, theirs_value, "today's wsp-api reads this crate's line: {ours}");
    let read_here: Frame = serde_json::from_str(&theirs).unwrap();
    assert_eq!(&read_here, f, "this crate reads today's line: {theirs}");
    assert_eq!(serde_json::to_string(&read_here).unwrap(), ours, "written again the same");
    serde_json::from_str(&ours).unwrap()
}

/// Today's bytes, frozen: each test's frames, in the order it writes them,
/// are the lines of `tests/golden/<test>.jsonl`, so the proof survives the
/// day `wsp-api` no longer has the hub's frames to compare with. The files
/// were written from today's `wsp-api` (each line is held to its bytes above)
/// with `WSP_WIRE_BLESS=1`.
fn golden(line: &str) {
    use std::cell::RefCell;
    use std::io::Write;
    thread_local! {
        static AT: RefCell<(String, usize, Option<Vec<String>>)> = const { RefCell::new((String::new(), 0, None)) };
    }
    let test = std::thread::current().name().unwrap_or("main").replace("::", "-");
    let file = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/golden").join(format!("{test}.jsonl"));
    let bless = std::env::var("WSP_WIRE_BLESS").is_ok_and(|v| v == "1");
    AT.with(|at| {
        let mut at = at.borrow_mut();
        if at.0 != test {
            *at = (test.clone(), 0, None);
        }
        let i = at.1;
        at.1 += 1;
        if bless {
            std::fs::create_dir_all(file.parent().unwrap()).unwrap();
            let mut f = std::fs::OpenOptions::new().create(true).write(true).append(i > 0).truncate(i == 0).open(&file).unwrap();
            writeln!(f, "{line}").unwrap();
            return;
        }
        let lines = at.2.get_or_insert_with(|| {
            std::fs::read_to_string(&file).unwrap_or_else(|e| panic!("{}: {e} (WSP_WIRE_BLESS=1 writes it)", file.display())).lines().map(str::to_string).collect()
        });
        assert_eq!(lines.get(i).map(String::as_str), Some(line), "{}:{}: today's frozen bytes", file.display(), i + 1);
    });
}

/// The bytes of the login frames in `frames()`, pinned as literals: this
/// repo's `wsp-api` has no `login`, `login_prompt` or `login_done` (the hub
/// repo's copy of it, which the hub serves, does). They are the bytes that
/// copy writes for these frames: its structs' fields in order, as its
/// `tests/round_trip.rs` and this test's hub-repo twin encode them.
fn login_line(f: &Frame) -> &'static str {
    match f {
        Frame::Login(l) if l.client == "wsp 0.1.0" => r#"{"type":"login","method":"github","client":"wsp 0.1.0","protocol":1}"#,
        Frame::LoginPrompt(l) if l.code == "ABCD-1234" => {
            r#"{"type":"login_prompt","url":"https://github.com/login/device","code":"ABCD-1234","expires_secs":900}"#
        }
        Frame::LoginDone(l) if l.user == "ann" => r#"{"type":"login_done","user":"ann","token":"t"}"#,
        other => panic!("no pinned bytes for {other:?}"),
    }
}

/// Checks a table: every frame is the same on both wires, its wire name is
/// the ordinal's, and every ordinal below `count` has a frame.
fn table(frames: &[Frame], count: usize, ordinal: fn(&Frame) -> (usize, &'static str), name: fn(&Value) -> String) {
    let mut seen = BTreeSet::new();
    for f in frames {
        let (n, wire_name) = ordinal(f);
        assert!(n < count, "{f:?}: ordinal {n} is not below {count}; bump it");
        assert_eq!(name(&wire(f)), wire_name, "{f:?}");
        seen.insert(n);
    }
    let missing: Vec<usize> = (0..count).filter(|n| !seen.contains(n)).collect();
    assert!(missing.is_empty(), "ordinals with no frame: {missing:?}");
}

fn s(v: &Value) -> String {
    v.as_str().unwrap_or_default().to_string()
}

// ---------------------------------------------------------------- fixtures

fn ann() -> Principal {
    Principal::Person("ann".into())
}
fn row() -> RowId {
    RowId("wsp-071".into())
}
fn ws() -> WorkspaceId {
    WorkspaceId("shared:team".into())
}
fn an_id(n: u8) -> ObjectId {
    ObjectId::of(&[n])
}
fn a_blob() -> Blob {
    Blob { id: ObjectId::of(b"{}"), bytes: b"{}".to_vec() }
}
fn odd_blob() -> Blob {
    // Bytes that need every kind of base64 padding, and a byte past ASCII.
    Blob { id: an_id(9), bytes: vec![0, 255, 7, 128] }
}
fn refs() -> Refs {
    let mut refs = Refs::new(ws());
    refs.set(Subject::Row(row()), an_id(1));
    refs.set(Subject::Lease(LeaseName("row:wsp-071".into())), an_id(2));
    refs
}

/// The hub's state as `state` and a snapshot carry it: users, token records,
/// shared workspaces, members and leases.
fn a_state() -> State {
    let mut st = State::default();
    let team = SharedName("team".into());
    for (name, admin, live) in [("ann", true, true), ("bo", false, false)] {
        let user = UserName(name.into());
        let state = if live { IdentityState::Active } else { IdentityState::Revoked };
        st.users.insert(user.clone(), User { name: user.clone(), state, admin });
        let id = TokenId(format!("t-{name}"));
        st.credentials.insert(
            id.clone(),
            Credential {
                id,
                user: user.clone(),
                hash: "ab".repeat(32),
                state,
                issued: At(1_763_400_000_000),
                revoked: (!live).then_some(At(1_763_400_100_000)),
            },
        );
    }
    st.workspaces.insert(team.clone(), SharedWorkspace { name: team.clone() });
    st.members.insert(team, BTreeSet::from([UserName("ann".into())]));
    let lease = LeaseName(r#"hubrow:["shared:team","wsp-071"]"#.into());
    st.leases.insert(lease.clone(), Lease { name: lease, holder: ann(), token: Token(4), expires: At(1_800_000_000_000) });
    st
}

/// The events the hub's stream carries.
fn events() -> Vec<Event> {
    let shared = |n: &str| wsp_store_contract::WorkspaceId::Shared(SharedName(n.into()));
    let e = |id: u64, kind, subject, payload: Value, workspace| Event {
        id: EventId(id),
        at: At(1_763_400_000_000 + id),
        principal: ann(),
        kind,
        subject,
        workspace,
        payload,
    };
    vec![
        e(
            401,
            EventKind::RefMoved,
            Subject::Row(row()),
            json!({ "workspace": "shared:team", "subject": { "row": "wsp-071" }, "to": an_id(1) }),
            shared("team"),
        ),
        e(
            402,
            EventKind::LeaseExpired,
            Subject::Lease(LeaseName("row:wsp-071".into())),
            json!({ "workspace": "shared:team", "row": "wsp-071", "holder": { "person": "ann" }, "token": 4 }),
            shared("team"),
        ),
        e(403, EventKind::OrderQueued, Subject::User(UserName("bo".into())), json!({ "user": "bo", "id": "hm-1" }), shared("team")),
        e(404, EventKind::HubTokenRevoked, Subject::Credential(TokenId("t-bo".into())), Value::Null, Default::default()),
        e(
            405,
            EventKind::HubMemberSet,
            Subject::Member { workspace: SharedName("team".into()), user: UserName("bo".into()) },
            json!({ "member": true }),
            Default::default(),
        ),
    ]
}

fn request(id: u64, call: Call) -> Frame {
    let key = matches!(call, Call::Command(_)).then(|| IdempotencyKey(format!("k-{id}")));
    Frame::Request(Request { v: p::PROTOCOL, id: ReqId(id), key, by: ann(), call })
}
fn ok(id: u64, answer: Answer, events: Vec<u64>) -> Frame {
    Frame::Reply(Reply { id: ReqId(id), outcome: Outcome::Ok(answer), events: events.into_iter().map(EventId).collect() })
}
fn err(id: u64, code: Code, refusal: Option<Refusal>) -> Frame {
    Frame::Reply(Reply {
        id: ReqId(id),
        outcome: Outcome::Err(Failure { code, text: format!("{code:?}"), refusal }),
        events: vec![],
    })
}

// ---------------------------------------------------------------- frames

const FRAMES: usize = 9;

fn frame_ordinal(f: &Frame) -> (usize, &'static str) {
    match f {
        Frame::Hello(_) => (0, "hello"),
        Frame::Welcome(_) => (1, "welcome"),
        Frame::Refused(_) => (2, "refused"),
        Frame::Request(_) => (3, "request"),
        Frame::Reply(_) => (4, "reply"),
        Frame::Feed(_) => (5, "feed"),
        Frame::Login(_) => (6, "login"),
        Frame::LoginPrompt(_) => (7, "login_prompt"),
        Frame::LoginDone(_) => (8, "login_done"),
    }
}

fn frames() -> Vec<Frame> {
    vec![
        Frame::Hello(Hello { token: ApiToken("s3cret".into()), by: ann(), client: "wsp 0.1.0".into(), protocol: p::PROTOCOL }),
        Frame::Hello(Hello {
            token: ApiToken("t".into()),
            by: Principal::Wsp("start-check".into()),
            client: "with \"quotes\" and a\nnewline".into(),
            protocol: 2,
        }),
        Frame::Welcome(Welcome { server: "wsp-hub".into(), protocol: p::PROTOCOL, head: EventId(411), by: Some(ann()) }),
        Frame::Welcome(Welcome { server: "wsp-hub".into(), protocol: p::PROTOCOL, head: EventId(0), by: None }),
        Frame::Refused(Refused { code: Code::Unauthorised, text: "the token is revoked".into() }),
        Frame::Refused(Refused { code: Code::Malformed, text: String::new() }),
        request(1, Call::Verb(Verb::Whoami(NoArgs {}))),
        ok(1, Answer::Subscribed, vec![]),
        Frame::Feed(Feed::Beat { beat: EventId(411), at: At(1_763_400_000_000) }),
        Frame::Login(Login { method: p::LOGIN_GITHUB.into(), client: "wsp 0.1.0".into(), protocol: p::PROTOCOL }),
        Frame::LoginPrompt(LoginPrompt { url: "https://github.com/login/device".into(), code: "ABCD-1234".into(), expires_secs: 900 }),
        Frame::LoginDone(LoginDone { user: "ann".into(), token: ApiToken("t".into()) }),
    ]
}

#[test]
fn every_frame_type_is_todays_bytes() {
    table(&frames(), FRAMES, frame_ordinal, |v| s(&v["type"]));
}

// ---------------------------------------------------------------- verbs

const VERBS: usize = 4;
const COMMANDS: usize = 6;
const SYNCS: usize = 12;
const CALLS: usize = VERBS + COMMANDS + SYNCS + 1;

fn verb_ordinal(v: &Verb) -> (usize, &'static str) {
    match v {
        Verb::State(_) => (0, "state"),
        Verb::Events { .. } => (1, "events"),
        Verb::Subscribe(_) => (2, "subscribe"),
        Verb::Whoami(_) => (3, "whoami"),
    }
}

fn command_ordinal(c: &IdentityCommand) -> (usize, &'static str) {
    match c {
        IdentityCommand::HubUserAdd { .. } => (0, "hub.user.add"),
        IdentityCommand::HubUserRevoke { .. } => (1, "hub.user.revoke"),
        IdentityCommand::HubTokenIssue { .. } => (2, "hub.token.issue"),
        IdentityCommand::HubTokenRevoke { .. } => (3, "hub.token.revoke"),
        IdentityCommand::HubWorkspaceAdd { .. } => (4, "hub.workspace.add"),
        IdentityCommand::HubMemberSet { .. } => (5, "hub.member.set"),
    }
}

fn sync_ordinal(c: &SyncCall) -> (usize, &'static str) {
    match c {
        SyncCall::Heads { .. } => (0, "sync.heads"),
        SyncCall::Have { .. } => (1, "sync.have"),
        SyncCall::Put { .. } => (2, "sync.put"),
        SyncCall::Get { .. } => (3, "sync.get"),
        SyncCall::Move(_) => (4, "refs.move"),
        SyncCall::Alloc(_) => (5, "ids.alloc"),
        SyncCall::LeaseTake(_) => (6, "lease.take"),
        SyncCall::LeaseRenew(_) => (7, "lease.renew"),
        SyncCall::LeaseRelease(_) => (8, "lease.release"),
        SyncCall::MsgSend(_) => (9, "msg.send"),
        SyncCall::MsgTake(_) => (10, "msg.take"),
        SyncCall::MsgAck { .. } => (11, "msg.ack"),
    }
}

fn call_of(f: &Frame) -> &Call {
    match f {
        Frame::Request(r) => &r.call,
        f => panic!("not a request: {f:?}"),
    }
}

fn call_ordinal(f: &Frame) -> (usize, &'static str) {
    match call_of(f) {
        Call::Verb(v) => verb_ordinal(v),
        Call::Command(c) => {
            let (n, name) = command_ordinal(c);
            (VERBS + n, name)
        }
        Call::Sync(c) => {
            let (n, name) = sync_ordinal(c);
            (VERBS + COMMANDS + n, name)
        }
        Call::Unknown { .. } => (VERBS + COMMANDS + SYNCS, "seat.teleport"),
    }
}

fn requests() -> Vec<Frame> {
    let verbs = vec![
        Verb::State(NoArgs {}),
        Verb::Events { from: EventId(400), limit: Some(50) },
        // A subscriber's keep-alive.
        Verb::Events { from: EventId(400), limit: Some(0) },
        Verb::Events { from: EventId(0), limit: None },
        Verb::Subscribe(Subscribe { from: Some(EventId(400)) }),
        Verb::Subscribe(Subscribe { from: None }),
        Verb::Whoami(NoArgs {}),
    ];
    let commands = vec![
        IdentityCommand::HubUserAdd { name: UserName("ann".into()), admin: true },
        IdentityCommand::HubUserAdd { name: UserName("bo".into()), admin: false },
        IdentityCommand::HubUserRevoke { name: UserName("bo".into()) },
        IdentityCommand::HubTokenIssue { id: TokenId("t-1".into()), user: UserName("ann".into()), hash: "ab".repeat(32) },
        IdentityCommand::HubTokenRevoke { id: TokenId("t-1".into()) },
        IdentityCommand::HubWorkspaceAdd { name: SharedName("team".into()) },
        IdentityCommand::HubMemberSet { workspace: SharedName("team".into()), user: UserName("bo".into()), member: true },
        IdentityCommand::HubMemberSet { workspace: SharedName("team".into()), user: UserName("bo".into()), member: false },
    ];
    let syncs = vec![
        SyncCall::Heads { workspace: ws() },
        SyncCall::Have { ids: vec![an_id(1), an_id(2)] },
        SyncCall::Have { ids: vec![] },
        SyncCall::Put { objects: vec![a_blob(), odd_blob()] },
        SyncCall::Get { ids: vec![an_id(1)] },
        SyncCall::Move(RefMove { workspace: ws(), subject: Subject::Row(row()), from: None, to: an_id(1) }),
        SyncCall::Move(RefMove {
            workspace: ws(),
            subject: Subject::Lease(LeaseName("row:wsp-071".into())),
            from: Some(an_id(1)),
            to: an_id(2),
        }),
        SyncCall::Alloc(IdsAlloc { workspace: ws(), project: "wsp".into(), n: 3, past: Some(184) }),
        SyncCall::Alloc(IdsAlloc { workspace: ws(), project: "wsp".into(), n: 1, past: None }),
        SyncCall::LeaseTake(LeaseTake { workspace: ws(), row: row() }),
        SyncCall::LeaseRenew(LeaseHold { row: row(), token: Token(3) }),
        SyncCall::LeaseRelease(LeaseHold { row: RowId("drive:wsp-list".into()), token: Token(3) }),
        SyncCall::MsgSend(MsgSend {
            to: "seat:wsp-071".into(),
            from: "seat:wsp-470".into(),
            seat: Some("%3".into()),
            text: "is it landed?".into(),
            re: Some("hm-1".into()),
        }),
        SyncCall::MsgSend(MsgSend { to: "primary:bo".into(), from: "person".into(), seat: None, text: "hi".into(), re: None }),
        SyncCall::MsgTake(NoArgs {}),
        SyncCall::MsgAck { ids: vec!["hm-1".into(), "hm-2".into()] },
    ];
    let mut out: Vec<Call> = vec![];
    out.extend(verbs.into_iter().map(Call::Verb));
    out.extend(commands.into_iter().map(Call::Command));
    out.extend(syncs.into_iter().map(Call::Sync));
    out.push(Call::Unknown { verb: "seat.teleport".into(), args: json!({ "seat": "cpd-1", "to": "moon" }) });
    out.into_iter().enumerate().map(|(i, c)| request(i as u64 + 1, c)).collect()
}

#[test]
fn every_request_verb_is_todays_bytes() {
    table(&requests(), CALLS, call_ordinal, |v| s(&v["verb"]));
}

/// The tables of names are the variants, in order, and each is the part of
/// today's table a hub serves: the sync verbs whole, and of the state core's
/// reads and commands only those a hub answers.
#[test]
fn the_verb_tables_are_the_variants_and_todays() {
    let (mut reads, mut commands, mut syncs) = (vec![], vec![], vec![]);
    for f in requests() {
        match call_of(&f) {
            Call::Verb(v) => reads.push(verb_ordinal(v).1),
            Call::Command(c) => {
                assert_eq!(c.verb(), command_ordinal(c).1);
                commands.push(c.verb());
            }
            Call::Sync(c) => {
                assert_eq!(c.verb(), sync_ordinal(c).1);
                syncs.push(c.verb());
            }
            Call::Unknown { verb, .. } => {
                for table in [p::READ_VERBS, p::COMMAND_VERBS, p::SYNC_VERBS, api::READ_VERBS, api::COMMAND_VERBS, api::SYNC_VERBS] {
                    assert!(!table.contains(&verb.as_str()), "{verb}");
                }
            }
        }
    }
    reads.dedup();
    commands.dedup();
    syncs.dedup();
    assert_eq!(reads, p::READ_VERBS);
    assert_eq!(commands, p::COMMAND_VERBS);
    assert_eq!(syncs, p::SYNC_VERBS);
    assert_eq!((p::READ_VERBS.len(), p::COMMAND_VERBS.len(), p::SYNC_VERBS.len()), (VERBS, COMMANDS, SYNCS));
    assert!(p::READ_VERBS.iter().all(|v| api::READ_VERBS.contains(v)));
    assert!(p::COMMAND_VERBS.iter().all(|v| api::COMMAND_VERBS.contains(v)));
    assert_eq!(p::SYNC_VERBS, api::SYNC_VERBS);
}

// ---------------------------------------------------------------- replies

const ANSWERS: usize = 16;

fn answer_ordinal(f: &Frame) -> (usize, &'static str) {
    let Frame::Reply(Reply { outcome: Outcome::Ok(a), .. }) = f else { panic!("not an ok reply: {f:?}") };
    match a {
        Answer::State { .. } => (0, "state"),
        Answer::Events { .. } => (1, "events"),
        Answer::Subscribed => (2, "subscribed"),
        Answer::Applied => (3, "applied"),
        Answer::Whoami { .. } => (4, "whoami"),
        Answer::Heads(_) => (5, "heads"),
        Answer::Missing(_) => (6, "missing"),
        Answer::Stored { .. } => (7, "stored"),
        Answer::Objects(_) => (8, "objects"),
        Answer::Move(_) => (9, "move"),
        Answer::Ids(_) => (10, "ids"),
        Answer::Lease { .. } => (11, "lease"),
        Answer::Released => (12, "released"),
        Answer::Queued { .. } => (13, "queued"),
        Answer::Messages(_) => (14, "messages"),
        Answer::Acked { .. } => (15, "acked"),
    }
}

/// The name an `ok` is tagged by: a unit answer is a bare string.
fn ok_name(v: &Value) -> String {
    match &v["ok"] {
        Value::String(name) => name.clone(),
        Value::Object(m) if m.len() == 1 => m.keys().next().unwrap().clone(),
        other => panic!("ok is not one tagged answer: {other}"),
    }
}

fn a_message(seat: Option<&str>, re: Option<&str>) -> HubMessage {
    HubMessage {
        id: "hm-1".into(),
        to: "seat:wsp-071".into(),
        user: "bo".into(),
        from_user: "ann".into(),
        from: "seat:wsp-470".into(),
        seat: seat.map(str::to_string),
        text: "is it landed?".into(),
        re: re.map(str::to_string),
        at: At(1_800_000_000_000),
    }
}

fn answers() -> Vec<Frame> {
    vec![
        ok(1, Answer::State { state: a_state(), at: EventId(411) }, vec![]),
        ok(2, Answer::State { state: State::default(), at: EventId(0) }, vec![]),
        ok(3, Answer::Events { events: events(), head: EventId(411), through: EventId(405) }, vec![]),
        // The keep-alive's answer: no event.
        ok(4, Answer::Events { events: vec![], head: EventId(411), through: EventId(400) }, vec![]),
        ok(5, Answer::Subscribed, vec![]),
        ok(6, Answer::Applied, vec![412, 413]),
        ok(7, Answer::Applied, vec![]),
        ok(8, Answer::Whoami { by: ann(), server: "wsp-hub".into() }, vec![]),
        ok(9, Answer::Heads(refs()), vec![]),
        ok(10, Answer::Heads(Refs::new(ws())), vec![]),
        ok(11, Answer::Missing(vec![an_id(2)]), vec![]),
        ok(12, Answer::Stored { stored: 1 }, vec![]),
        ok(13, Answer::Objects(vec![a_blob(), odd_blob()]), vec![]),
        ok(14, Answer::Move(Move::Moved { head: an_id(2) }), vec![]),
        ok(15, Answer::Move(Move::Stale { head: Some(an_id(1)) }), vec![]),
        ok(16, Answer::Move(Move::Stale { head: None }), vec![]),
        ok(17, Answer::Ids(vec!["wsp-185".into(), "wsp-186".into()]), vec![]),
        ok(18, Answer::Ids(vec![]), vec![]),
        ok(19, Answer::Lease { token: Token(4), expires: At(1_800_000_000_000) }, vec![]),
        ok(20, Answer::Released, vec![]),
        ok(21, Answer::Queued { id: "hm-1".into(), user: "bo".into() }, vec![]),
        ok(22, Answer::Messages(vec![a_message(Some("%3"), Some("hm-0")), a_message(None, None)]), vec![]),
        ok(23, Answer::Messages(vec![]), vec![]),
        ok(24, Answer::Acked { acked: 1 }, vec![]),
    ]
}

#[test]
fn every_answer_is_todays_bytes() {
    table(&answers(), ANSWERS, answer_ordinal, ok_name);
}

// ---------------------------------------------------------------- codes

const CODES: usize = 16;

fn code_ordinal(c: Code) -> (usize, &'static str) {
    match c {
        Code::Stale => (0, "stale"),
        Code::TooOld => (1, "too_old"),
        Code::Conflict => (2, "conflict"),
        Code::Held => (3, "held"),
        Code::NotFound => (4, "not_found"),
        Code::Illegal => (5, "illegal"),
        Code::Forbidden => (6, "forbidden"),
        Code::Invalid => (7, "invalid"),
        Code::Malformed => (8, "malformed"),
        Code::Unauthorised => (9, "unauthorised"),
        Code::Protocol => (10, "protocol"),
        Code::Unsupported => (11, "unsupported"),
        Code::Backend => (12, "backend"),
        Code::Fenced => (13, "fenced"),
        Code::TooLarge => (14, "too_large"),
        Code::Unknown => (15, "unknown"),
    }
}

fn code_of(f: &Frame) -> Code {
    match f {
        Frame::Reply(Reply { outcome: Outcome::Err(e), .. }) => e.code,
        Frame::Refused(r) => r.code,
        f => panic!("no code: {f:?}"),
    }
}

fn code_name(v: &Value) -> String {
    s(if v["type"] == "refused" { &v["code"] } else { &v["err"]["code"] })
}

/// One error of every code, as the hub sends them: the contract's eight with
/// the refusal whole and without, and the codes a hello or a login is
/// refused with also as `refused`.
fn errors() -> Vec<Frame> {
    let lease = LeaseName("row:wsp-071".into());
    vec![
        err(1, Code::Stale, Some(Refusal::Stale { lease: lease.clone(), current: Token(4) })),
        err(2, Code::TooOld, Some(Refusal::TooOld { oldest: EventId(100) })),
        err(3, Code::Conflict, Some(Refusal::Conflict { current: Rev("r9".into()) })),
        err(4, Code::Held, Some(Refusal::Held { lease: lease.clone(), holder: Principal::Person("bo".into()) })),
        err(5, Code::NotFound, Some(Refusal::NotFound { subject: Subject::Lease(lease) })),
        // A `get` that serves nothing: not_found with no refusal.
        err(6, Code::NotFound, None),
        err(
            7,
            Code::Illegal,
            Some(Refusal::Illegal {
                subject: Subject::User(UserName("bo".into())),
                state: "revoked".into(),
                verb: "hub.token.issue".into(),
            }),
        ),
        err(8, Code::Forbidden, Some(Refusal::Forbidden { by: ann() })),
        // A message verb's forbidden carries none.
        err(9, Code::Forbidden, None),
        err(10, Code::Invalid, Some(Refusal::Invalid { reason: "no key".into() })),
        err(11, Code::Invalid, None),
        err(12, Code::Malformed, None),
        Frame::Refused(Refused { code: Code::Malformed, text: "the first frame must be hello".into() }),
        err(13, Code::Unauthorised, None),
        Frame::Refused(Refused { code: Code::Unauthorised, text: String::new() }),
        err(14, Code::Protocol, None),
        Frame::Refused(Refused { code: Code::Protocol, text: "this server speaks protocol 1, not 2".into() }),
        err(15, Code::Unsupported, None),
        Frame::Refused(Refused { code: Code::Unsupported, text: "this hub has no login".into() }),
        err(16, Code::Backend, None),
        Frame::Refused(Refused { code: Code::Backend, text: "8 logins are running".into() }),
        err(17, Code::Fenced, None),
        err(18, Code::TooLarge, None),
        err(19, Code::Unknown, None),
        Frame::Refused(Refused { code: Code::Forbidden, text: "ann is not on this hub's list".into() }),
    ]
}

#[test]
fn every_error_code_is_todays_bytes() {
    table(&errors(), CODES, |f| code_ordinal(code_of(f)), code_name);
}

/// The first eight codes are spelled as the contract's refusal codes.
#[test]
fn the_contract_codes_are_its_refusals_codes() {
    for f in errors() {
        let Frame::Reply(Reply { outcome: Outcome::Err(Failure { code, refusal: Some(r), .. }), .. }) = &f else {
            continue;
        };
        let tag = serde_json::to_value(r).unwrap()["code"].clone();
        assert_eq!(tag, json!(code_ordinal(*code).1), "{f:?}");
        assert_eq!(serde_json::from_value::<Code>(tag).unwrap(), *code);
    }
}

// ---------------------------------------------------------------- feed

const FEEDS: usize = 6;

fn feed_ordinal(f: &Frame) -> (usize, &'static str) {
    let Frame::Feed(feed) = f else { panic!("not a feed frame: {f:?}") };
    match feed {
        Feed::State { .. } => (0, "state"),
        Feed::Event(_) => (1, "event"),
        Feed::Beat { .. } => (2, "beat"),
        Feed::Part { .. } => (3, "part"),
        Feed::Resync { .. } => (4, "resync"),
        Feed::Gone { .. } => (5, "gone"),
    }
}

fn feeds() -> Vec<Frame> {
    let mut out = vec![
        Frame::Feed(Feed::State { state: a_state(), at: EventId(411) }),
        Frame::Feed(Feed::Beat { beat: EventId(412), at: At(1_763_400_010_000) }),
        Frame::Feed(Feed::Part { part: a_state(), at: EventId(411) }),
        Frame::Feed(Feed::Resync { state: a_state(), at: EventId(413) }),
        Frame::Feed(Feed::Gone { subject: Subject::Row(row()), at: EventId(414) }),
    ];
    out.extend(events().into_iter().map(|e| Frame::Feed(Feed::Event(e))));
    out
}

#[test]
fn every_feed_frame_is_todays_bytes() {
    table(&feeds(), FEEDS, feed_ordinal, |v| {
        let mut keys: Vec<&String> = v.as_object().unwrap().keys().collect();
        keys.retain(|k| *k != "type");
        assert_eq!(keys.len(), 1, "{v}");
        keys[0].clone()
    });
}

// ---------------------------------------------------------------- what a hub does not serve

fn decode(line: &str) -> Frame {
    serde_json::from_str(line).unwrap_or_else(|e| panic!("{line}: {e}"))
}

fn unknown(line: &str) -> (u64, String, Value) {
    match decode(line) {
        Frame::Request(Request { id, call: Call::Unknown { verb, args }, .. }) => (id.0, verb, args),
        other => panic!("{line} decoded as {other:?}, not an unknown verb"),
    }
}

/// Every read verb and command of today's state core that a hub does not
/// serve decodes here as `Call::Unknown`, keeping its id and its `args`, so
/// the hub can answer it; so does a hub verb whose `args` do not fit.
#[test]
fn the_state_cores_other_verbs_are_unknown_here_and_keep_their_id() {
    let mut n = 0;
    for verb in api::READ_VERBS.iter().chain(api::COMMAND_VERBS).filter(|v| !p::READ_VERBS.contains(v) && !p::COMMAND_VERBS.contains(v)) {
        n += 1;
        let line = format!(r#"{{"type":"request","v":1,"id":{n},"by":"person:ann","verb":"{verb}","args":{{"row":"wsp-1"}}}}"#);
        assert_eq!(unknown(&line), (n, verb.to_string(), json!({ "row": "wsp-1" })), "{verb}");
    }
    assert_eq!(n as usize, api::READ_VERBS.len() - p::READ_VERBS.len() + api::COMMAND_VERBS.len() - p::COMMAND_VERBS.len());

    // Frames today's wsp-api writes for two of them, whole.
    let rows = api::Frame::Request(api::Request {
        v: 1,
        id: api::ReqId(70),
        key: None,
        by: ann(),
        call: api::Call::Verb(api::Verb::Rows(wsp_store_contract::RowFilter { limit: Some(5), ..Default::default() })),
    });
    let line = serde_json::to_string(&rows).unwrap();
    let (id, verb, args) = unknown(&line);
    assert_eq!((id, verb.as_str()), (70, "rows"));
    assert_eq!(args, serde_json::to_value(&serde_json::from_str::<Value>(&line).unwrap()["args"]).unwrap());
    let add = api::Frame::Request(api::Request {
        v: 1,
        id: api::ReqId(71),
        key: Some(IdempotencyKey("k".into())),
        by: ann(),
        call: api::Call::Command(wsp_store_contract::Command::OrderSend { to: SeatId("cpd-1".into()), text: "go".into(), first: false }),
    });
    assert_eq!(unknown(&serde_json::to_string(&add).unwrap()).1, "order.send");

    // A verb of this crate whose args do not fit: unknown too, its id kept.
    for (line, verb) in [
        (r#"{"type":"request","v":1,"id":80,"by":"person:ann","verb":"events","args":{}}"#, "events"),
        (r#"{"type":"request","v":1,"id":81,"by":"person:ann","verb":"lease.take","args":{"row":"wsp-1"}}"#, "lease.take"),
        (r#"{"type":"request","v":1,"id":82,"by":"person:ann","verb":"hub.user.add","args":{}}"#, "hub.user.add"),
    ] {
        let (id, got, _) = unknown(line);
        assert_eq!(got, verb);
        assert!(id >= 80);
    }
}

/// An answer only the state core gives is no answer here: a hub never sends
/// one, since it never takes the verb.
#[test]
fn the_state_cores_other_answers_do_not_decode_here() {
    for answer in [api::Answer::Rows(vec![]), api::Answer::Wip { state: State::default(), at: EventId(1) }, api::Answer::Throttle(json!({}))] {
        let line = serde_json::to_string(&api::Frame::Reply(api::Reply { id: api::ReqId(1), outcome: api::Outcome::Ok(answer), events: vec![] })).unwrap();
        assert!(serde_json::from_str::<Frame>(&line).is_err(), "{line}");
    }
}

// ---------------------------------------------------------------- other peers

/// A verb with no arguments reads `args` left out, `null` or `{}` (with
/// fields a newer peer added), here as today; and it is written `{}`.
#[test]
fn a_verb_without_arguments_decodes_either_way() {
    for args in [r#""#, r#","args":null"#, r#","args":{}"#, r#","args":{"newer":1}"#] {
        for (verb, want) in [("state", Call::Verb(Verb::State(NoArgs {}))), ("whoami", Call::Verb(Verb::Whoami(NoArgs {}))), ("msg.take", Call::Sync(SyncCall::MsgTake(NoArgs {})))] {
            let line = format!(r#"{{"type":"request","v":1,"id":3,"by":"person:ann","verb":"{verb}"{args}}}"#);
            assert_eq!(call_of(&decode(&line)), &want, "{line}");
            let today: api::Frame = serde_json::from_str(&line).unwrap();
            assert_eq!(serde_json::to_string(&today).unwrap(), serde_json::to_string(&decode(&line)).unwrap(), "{line}");
        }
    }
    let line = r#"{"type":"request","v":1,"id":3,"by":"person:ann","verb":"subscribe","args":{}}"#;
    assert_eq!(call_of(&decode(line)), &Call::Verb(Verb::Subscribe(Subscribe { from: None })));
}

/// Fields an older peer leaves out, and codes a newer one adds.
#[test]
fn peers_of_other_versions_decode() {
    let welcome = decode(r#"{"type":"welcome","server":"wsp-hub","protocol":1,"head":4}"#);
    assert_eq!(welcome, Frame::Welcome(Welcome { server: "wsp-hub".into(), protocol: 1, head: EventId(4), by: None }));
    let refused = decode(r#"{"type":"refused","code":"unauthorised"}"#);
    assert_eq!(refused, Frame::Refused(Refused { code: Code::Unauthorised, text: String::new() }));
    let newer = decode(r#"{"type":"refused","code":"quota","text":"x"}"#);
    assert_eq!(code_of(&newer), Code::Unknown);
    let events = decode(r#"{"type":"reply","id":2,"ok":{"events":{"events":[],"head":9}}}"#);
    assert_eq!(events, ok(2, Answer::Events { events: vec![], head: EventId(9), through: EventId(0) }, vec![]));
    let failure = decode(r#"{"type":"reply","id":2,"err":{"code":"fenced"}}"#);
    assert_eq!(failure, Frame::Reply(Reply { id: ReqId(2), outcome: Outcome::Err(Failure { code: Code::Fenced, text: String::new(), refusal: None }), events: vec![] }));
}

#[test]
fn a_token_is_not_printed() {
    assert_eq!(format!("{:?}", ApiToken("s3cret".into())), "ApiToken(..)");
    let done = format!("{:?}", LoginDone { user: "ann".into(), token: ApiToken("s3cret".into()) });
    assert!(!done.contains("s3cret") && done.contains("ann"), "{done}");
}

// ---------------------------------------------------------------- principal

#[test]
fn every_principal_kind_is_todays_text_form() {
    let every = [
        (Principal::Person("ann".into()), "person:ann"),
        (Principal::Seat(SeatId("cpd-3698".into())), "seat:cpd-3698"),
        (Principal::Manager(MachineName("box".into())), "manager:box"),
        (Principal::Executor(MachineName("box".into())), "executor:box"),
        (Principal::Wsp("wsp-hub".into()), "wsp:wsp-hub"),
        (Principal::Person("a:b".into()), "person:a:b"),
    ];
    for (who, text) in &every {
        assert_eq!(p::principal::text(who), *text);
        assert_eq!(api::principal::text(who), *text);
        assert_eq!(&p::principal::parse(text).unwrap(), who);
    }
    for kind in p::principal::KINDS {
        assert!(every.iter().any(|(_, t)| t.starts_with(&format!("{kind}:"))), "{kind}");
    }
    assert_eq!(p::principal::KINDS, api::principal::KINDS);
    for bad in ["", "ann", "person:", "robot:x", ":ann"] {
        assert!(p::principal::parse(bad).is_err(), "{bad}");
        assert!(api::principal::parse(bad).is_err(), "{bad}: today's refuses it too");
    }
}

// ---------------------------------------------------------------- spelling

/// The wire spelling, pinned: a rename in the types is a protocol change.
#[test]
fn wire_spelling_is_pinned() {
    let take = request(9, Call::Sync(SyncCall::LeaseTake(LeaseTake { workspace: ws(), row: row() })));
    assert_eq!(
        serde_json::to_string(&take).unwrap(),
        r#"{"type":"request","v":1,"id":9,"by":"person:ann","verb":"lease.take","args":{"workspace":"shared:team","row":"wsp-071"}}"#
    );
    let add = request(10, Call::Command(IdentityCommand::HubUserAdd { name: UserName("bo".into()), admin: false }));
    assert_eq!(
        serde_json::to_string(&add).unwrap(),
        r#"{"type":"request","v":1,"id":10,"key":"k-10","by":"person:ann","verb":"hub.user.add","args":{"name":"bo","admin":false}}"#
    );
    let lease = ok(9, Answer::Lease { token: Token(4), expires: At(5) }, vec![]);
    assert_eq!(serde_json::to_string(&lease).unwrap(), r#"{"type":"reply","id":9,"ok":{"lease":{"token":4,"expires":5}},"events":[]}"#);
    let stale = ok(9, Answer::Move(Move::Stale { head: None }), vec![]);
    assert_eq!(serde_json::to_string(&stale).unwrap(), r#"{"type":"reply","id":9,"ok":{"move":{"stale":{"head":null}}},"events":[]}"#);
    let put = request(11, Call::Sync(SyncCall::Put { objects: vec![odd_blob()] }));
    let v = serde_json::to_value(&put).unwrap();
    assert_eq!(v["args"]["objects"][0]["bytes"], json!("AP8HgA=="));
    assert_eq!(v["args"]["objects"][0]["id"], json!(format!("sha256:{}", an_id(9).hex())));
    let done = Frame::LoginDone(LoginDone { user: "ann".into(), token: ApiToken("t".into()) });
    assert_eq!(serde_json::to_string(&done).unwrap(), r#"{"type":"login_done","user":"ann","token":"t"}"#);
    let mut keys: BTreeMap<&str, ()> = BTreeMap::new();
    for f in frames() {
        keys.insert(frame_ordinal(&f).1, ());
    }
    assert_eq!(keys.len(), FRAMES);
}

/// The constants a spoke and a hub both read are today's.
#[test]
fn the_constants_are_todays() {
    assert_eq!(p::PROTOCOL, api::PROTOCOL);
    assert_eq!(p::MAX_LINE, api::MAX_LINE);
    assert_eq!(p::MAX_EVENTS, api::MAX_EVENTS);
    assert_eq!(p::MAX_EVENTS_BYTES, api::MAX_EVENTS_BYTES);
    assert_eq!(p::MAX_IDS, api::MAX_IDS);
    // This repo's `wsp-api` has no `MAX_PAST` (the hub repo's copy added it with
    // the check that refuses a higher `past`): held to the hub repo's value.
    assert_eq!(p::MAX_PAST, 1_000_000_000);
    assert_eq!(p::FENCE_TOKEN, api::FENCE_TOKEN);
    // This repo's `wsp-api` has no login: `LOGIN_GITHUB` is held to its spelling.
    assert_eq!(p::LOGIN_GITHUB, "github");
    assert_eq!((p::BEAT_SECS, p::MISSED_BEATS), (api::BEAT_SECS, api::MISSED_BEATS));
    let every: Vec<u8> = (0..=255).collect();
    assert_eq!(p::sync::base64::encode(&every), api::sync::base64::encode(&every));
}

