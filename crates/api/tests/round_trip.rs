//! Every frame of the API round-trips, table-driven: every request verb (the
//! read verbs, `subscribe`, every command, an unknown verb), every answer,
//! every error code, hello/welcome/refused, and every `Feed` frame. Each
//! table's coverage is held by an exhaustive match (`ordinal`s below), so a new
//! variant (here or in the contract's `Command`) does not compile until it has
//! an arm; give it the next ordinal, bump the count, and add its frame.
//!
//! Then `Principal`'s text form, the pinned wire spelling, peers of other
//! versions, and the memory pair's framing (host-api's tests, copied).

use std::collections::{BTreeMap, BTreeSet};
use std::fmt::Debug;
use std::time::Duration;

use serde_json::{json, Value};
use wsp_api::*;
use wsp_store_contract::{
    At, Block, Claim, Entity, EventKind, Fence, GroupId, LeaseName, ListState, MachineName,
    ProjectId, ReleaseTo, RowState, SeatState, Subject, Token, Transport, Verdict,
};

// ---------------------------------------------------------------- fixtures

fn row_id() -> RowId {
    RowId("wsp-071".into())
}
fn seat_id() -> SeatId {
    SeatId("cpd-3698".into())
}
fn list_id() -> ListId {
    ListId("s1-state-core".into())
}
fn ed() -> Principal {
    Principal::Person("ed".into())
}
fn fence() -> Fence {
    Fence {
        lease: LeaseName("claim:wsp-071".into()),
        token: Token(3),
    }
}
fn a_row() -> Row {
    Row {
        id: row_id(),
        project: ProjectId("wsp".into()),
        title: "wsp-api".into(),
        state: RowState::Doing,
        before_block: None,
        seat: Some(seat_id()),
        group: Some(GroupId {
            list: list_id(),
            n: 2,
        }),
        commit: None,
        fields: BTreeMap::from([("tier".into(), "opus".into())]),
    }
}
fn a_seat() -> Seat {
    Seat {
        id: seat_id(),
        machine: MachineName("mac".into()),
        kind: "claude".into(),
        model: Some("opus".into()),
        state: SeatState::Working,
        before_lost: None,
        row: Some(row_id()),
        attempt: 1,
        pid: Some(4242),
        fields: BTreeMap::from([("role".into(), "member".into())]),
    }
}
fn a_list() -> List {
    List {
        id: list_id(),
        state: ListState::Running,
        groups: vec![wsp_store_contract::Group {
            id: GroupId {
                list: list_id(),
                n: 2,
            },
            state: ListState::Running,
            members: vec![row_id()],
            attempt: 0,
            fields: BTreeMap::new(),
        }],
        fields: BTreeMap::from([("caption".into(), "state core".into())]),
    }
}
fn an_order() -> Order {
    Order {
        id: wsp_store_contract::OrderId("ord-1".into()),
        from: ed(),
        to: seat_id(),
        text: "begin\nwith two lines".into(),
        state: OrderState::Delivered,
        attempts: 1,
    }
}
fn an_ask() -> Ask {
    Ask {
        id: wsp_store_contract::AskId("m-1".into()),
        kind: AskKind::Ask,
        from: Principal::Seat(seat_id()),
        to: Some(ed()),
        about: Some(Subject::Row(row_id())),
        text: "which?".into(),
        state: AskState::Raised,
        answer: None,
        fields: BTreeMap::new(),
    }
}
fn a_machine() -> Machine {
    Machine {
        name: MachineName("mac".into()),
        transport: Transport::Tcp {
            addr: "100.64.0.1:7411".into(),
        },
        beat: Some(At(1_763_400_000_000)),
        facts: BTreeMap::from([("load".into(), "1.5".into())]),
    }
}
fn an_event(id: u64) -> Event {
    Event {
        id: EventId(id),
        at: At(1_763_400_000_000 + id),
        principal: Principal::Wsp("main".into()),
        kind: EventKind::MirrorPut,
        subject: Subject::Seat(seat_id()),
        payload: serde_json::to_value(Entity::Seat(a_seat())).unwrap(),
    }
}
fn a_state() -> State {
    let mut s = State::default();
    s.rows.insert(row_id(), a_row());
    s.seats.insert(seat_id(), a_seat());
    s.lists.insert(list_id(), a_list());
    s.claims.insert(
        row_id(),
        Claim {
            row: row_id(),
            seat: seat_id(),
            token: Token(3),
            expires: At(1_763_400_060_000),
        },
    );
    s.asks.insert(an_ask().id.clone(), an_ask());
    s.machines.insert(a_machine().name.clone(), a_machine());
    s
}

fn request(id: u64, call: Call) -> Frame {
    let key = matches!(call, Call::Command(_)).then(|| IdempotencyKey(format!("k-{id}")));
    Frame::Request(Request {
        v: PROTOCOL,
        id: ReqId(id),
        key,
        by: Principal::Seat(seat_id()),
        call,
    })
}
fn ok(id: u64, answer: Answer, events: Vec<u64>) -> Frame {
    Frame::Reply(Reply {
        id: ReqId(id),
        outcome: Outcome::Ok(answer),
        events: events.into_iter().map(EventId).collect(),
    })
}
fn err(id: u64, code: Code, refusal: Option<Refusal>) -> Frame {
    Frame::Reply(Reply {
        id: ReqId(id),
        outcome: Outcome::Err(Failure {
            code,
            text: format!("{code:?}"),
            refusal,
        }),
        events: vec![],
    })
}
fn hello() -> Frame {
    Frame::Hello(Hello {
        token: ApiToken("s3cret".into()),
        by: ed(),
        client: "wsp 0.1.0".into(),
        protocol: PROTOCOL,
    })
}

// ---------------------------------------------------------------- the harness

/// One frame through the wire: one line, decoded back to itself, written
/// again to the same line. Returns its JSON.
fn trip(f: &Frame) -> Value {
    let line = serde_json::to_string(f).unwrap();
    assert!(!line.contains('\n'), "a frame is one line: {line}");
    let back: Frame = serde_json::from_str(&line).unwrap();
    assert_eq!(&back, f, "round trip of {line}");
    assert_eq!(serde_json::to_string(&back).unwrap(), line, "rewritten");
    serde_json::from_str(&line).unwrap()
}

/// Checks a table: every frame round-trips, its wire name is the ordinal's,
/// and every ordinal below `count` is in the table.
fn table<T: Debug>(
    frames: &[T],
    count: usize,
    frame: fn(&T) -> &Frame,
    ordinal: fn(&T) -> (usize, &'static str),
    name: fn(&Value) -> String,
) {
    let mut seen = BTreeSet::new();
    for t in frames {
        let (n, wire) = ordinal(t);
        assert!(
            n < count,
            "{t:?}: ordinal {n} is not below {count}; bump it"
        );
        assert_eq!(name(&trip(frame(t))), wire, "{t:?}");
        seen.insert(n);
    }
    let missing: Vec<usize> = (0..count).filter(|n| !seen.contains(n)).collect();
    assert!(missing.is_empty(), "ordinals with no frame: {missing:?}");
}

fn same(f: &Frame) -> &Frame {
    f
}
fn s(v: &Value) -> String {
    v.as_str().unwrap_or_default().to_string()
}

// ---------------------------------------------------------------- frames

const FRAMES: usize = 6;

fn frame_ordinal(f: &Frame) -> (usize, &'static str) {
    match f {
        Frame::Hello(_) => (0, "hello"),
        Frame::Welcome(_) => (1, "welcome"),
        Frame::Refused(_) => (2, "refused"),
        Frame::Request(_) => (3, "request"),
        Frame::Reply(_) => (4, "reply"),
        Frame::Feed(_) => (5, "feed"),
    }
}

fn frames() -> Vec<Frame> {
    vec![
        hello(),
        Frame::Welcome(Welcome {
            server: "main".into(),
            protocol: PROTOCOL,
            head: EventId(411),
        }),
        Frame::Refused(Refused {
            code: Code::Unauthorised,
            text: "wrong token".into(),
        }),
        request(1, Call::Verb(Verb::State(NoArgs {}))),
        ok(1, Answer::Subscribed, vec![]),
        Frame::Feed(Feed::Beat {
            beat: EventId(411),
            at: At(1_763_400_000_000),
        }),
    ]
}

#[test]
fn every_frame_type_round_trips() {
    table(&frames(), FRAMES, same, frame_ordinal, |v| s(&v["type"]));
}

// ---------------------------------------------------------------- verbs

/// Read verbs, then the contract's commands, then the unknown verb.
const VERBS: usize = 13;
const COMMANDS: usize = 20;
const CALLS: usize = VERBS + COMMANDS + 1;

fn verb_ordinal(v: &Verb) -> (usize, &'static str) {
    match v {
        Verb::State(_) => (0, "state"),
        Verb::Row { .. } => (1, "row"),
        Verb::Rows(_) => (2, "rows"),
        Verb::List { .. } => (3, "list"),
        Verb::Lists(_) => (4, "lists"),
        Verb::Seat { .. } => (5, "seat"),
        Verb::Seats(_) => (6, "seats"),
        Verb::Orders { .. } => (7, "orders"),
        Verb::Asks { .. } => (8, "asks"),
        Verb::Machines(_) => (9, "machines"),
        Verb::Events { .. } => (10, "events"),
        Verb::Subscribe(_) => (11, "subscribe"),
        Verb::Wip(_) => (12, "wip"),
    }
}

fn command_ordinal(c: &Command) -> (usize, &'static str) {
    match c {
        Command::RowAdd { .. } => (0, "row.add"),
        Command::RowEdit { .. } => (1, "row.edit"),
        Command::RowClaim { .. } => (2, "row.claim"),
        Command::RowReview { .. } => (3, "row.review"),
        Command::RowBlock { .. } => (4, "row.block"),
        Command::RowPark { .. } => (5, "row.park"),
        Command::RowLand { .. } => (6, "row.land"),
        Command::ListGo { .. } => (7, "list.go"),
        Command::ListHold { .. } => (8, "list.hold"),
        Command::ListRelease { .. } => (9, "list.release"),
        Command::SeatSpawn { .. } => (10, "seat.spawn"),
        Command::SeatEnd { .. } => (11, "seat.end"),
        Command::SeatDespawn { .. } => (12, "seat.despawn"),
        Command::OrderSend { .. } => (13, "order.send"),
        Command::OrderCancel { .. } => (14, "order.cancel"),
        Command::AskRaise { .. } => (15, "ask.raise"),
        Command::AskAnswer { .. } => (16, "ask.answer"),
        Command::AskAck { .. } => (17, "ask.ack"),
        Command::MachineSet { .. } => (18, "machine.set"),
        Command::Mirror { .. } => (19, "mirror"),
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
        Call::Unknown { .. } => (VERBS + COMMANDS, "seat.teleport"),
    }
}

fn requests() -> Vec<Frame> {
    let verbs = vec![
        Verb::State(NoArgs {}),
        Verb::Row { row: row_id() },
        Verb::Rows(RowFilter {
            project: Some(ProjectId("wsp".into())),
            states: vec![RowState::Doing, RowState::Review],
            list: Some(list_id()),
            seat: None,
        }),
        Verb::List { list: list_id() },
        Verb::Lists(NoArgs {}),
        Verb::Seat { seat: seat_id() },
        Verb::Seats(SeatFilter {
            machine: Some(MachineName("mac".into())),
            states: vec![SeatState::Working],
            row: None,
        }),
        Verb::Orders {
            seat: Some(seat_id()),
            states: vec![OrderState::Queued, OrderState::Typed],
        },
        Verb::Asks {
            kinds: vec![AskKind::Ask, AskKind::Flag],
            states: vec![AskState::Raised],
        },
        Verb::Machines(NoArgs {}),
        Verb::Events {
            from: EventId(400),
            limit: Some(50),
        },
        Verb::Subscribe(Subscribe {
            from: Some(EventId(400)),
        }),
        Verb::Wip(NoArgs {}),
    ];
    let commands = vec![
        Command::RowAdd {
            project: ProjectId("wsp".into()),
            title: "S1".into(),
            fields: BTreeMap::from([("tier".into(), "opus".into())]),
        },
        Command::RowEdit {
            row: row_id(),
            title: Some("renamed".into()),
            fields: BTreeMap::from([("tier".into(), None)]),
        },
        Command::RowClaim {
            row: row_id(),
            seat: seat_id(),
            fence: fence(),
        },
        Command::RowReview {
            row: row_id(),
            commit: "952409c".into(),
            fence: fence(),
        },
        Command::RowBlock {
            row: row_id(),
            block: Block::On {
                reason: "waits on wsp-070".into(),
            },
        },
        Command::RowPark { row: row_id() },
        Command::RowLand {
            row: row_id(),
            verdict: Verdict::Blocks {
                reason: "red".into(),
            },
            fence: fence(),
        },
        Command::ListGo { list: list_id() },
        Command::ListHold {
            list: list_id(),
            group: None,
            reason: "look first".into(),
        },
        Command::ListRelease {
            list: list_id(),
            group: 2,
            to: ReleaseTo::Barrier,
        },
        Command::SeatSpawn {
            row: Some(row_id()),
            attempt: 1,
            machine: None,
            kind: "claude".into(),
            model: Some("opus".into()),
        },
        Command::SeatEnd {
            seat: seat_id(),
            fence: None,
        },
        Command::SeatDespawn {
            seat: seat_id(),
            lost: true,
        },
        Command::OrderSend {
            to: seat_id(),
            text: "go".into(),
        },
        Command::OrderCancel {
            order: an_order().id,
        },
        Command::AskRaise {
            kind: AskKind::Note,
            to: Some(ed()),
            about: Some(Subject::Row(row_id())),
            text: "fyi".into(),
        },
        Command::AskAnswer {
            ask: an_ask().id,
            text: "this".into(),
        },
        Command::AskAck { ask: an_ask().id },
        Command::MachineSet {
            name: MachineName("mac".into()),
            transport: Some(Transport::Local),
            facts: BTreeMap::new(),
        },
        Command::Mirror {
            put: vec![Entity::Seat(a_seat()), Entity::Row(a_row())],
            gone: vec![Subject::Order(an_order().id)],
        },
    ];
    let mut out: Vec<Call> = vec![];
    out.extend(verbs.into_iter().map(Call::Verb));
    out.extend(commands.into_iter().map(Call::Command));
    out.push(Call::Unknown {
        verb: "seat.teleport".into(),
        args: json!({"seat": "cpd-3698", "to": "moon"}),
    });
    out.into_iter()
        .enumerate()
        .map(|(i, c)| request(i as u64 + 1, c))
        .collect::<Vec<_>>()
}

#[test]
fn every_request_verb_round_trips() {
    table(&requests(), CALLS, same, call_ordinal, |v| s(&v["verb"]));
}

/// The two tables of names are the variants, in order: a verb added to
/// `Verb` or to the contract's `Command` fails here until it is listed.
#[test]
fn the_verb_tables_are_the_variants() {
    let mut reads = vec![];
    let mut commands = vec![];
    for f in requests() {
        match call_of(&f) {
            Call::Verb(v) => reads.push(verb_ordinal(v).1),
            Call::Command(c) => commands.push(command_ordinal(c).1),
            Call::Unknown { verb, .. } => {
                assert!(!READ_VERBS.contains(&verb.as_str()));
                assert!(!COMMAND_VERBS.contains(&verb.as_str()));
            }
        }
    }
    assert_eq!(reads, READ_VERBS);
    assert_eq!(commands, COMMAND_VERBS);
    assert_eq!(READ_VERBS.len(), VERBS);
    assert_eq!(COMMAND_VERBS.len(), COMMANDS);
}

// ---------------------------------------------------------------- replies

const ANSWERS: usize = 14;

fn answer_ordinal(f: &Frame) -> (usize, &'static str) {
    let Frame::Reply(Reply {
        outcome: Outcome::Ok(a),
        ..
    }) = f
    else {
        panic!("not an ok reply: {f:?}")
    };
    match a {
        Answer::State { .. } => (0, "state"),
        Answer::Row(_) => (1, "row"),
        Answer::Rows(_) => (2, "rows"),
        Answer::List(_) => (3, "list"),
        Answer::Lists(_) => (4, "lists"),
        Answer::Seat(_) => (5, "seat"),
        Answer::Seats(_) => (6, "seats"),
        Answer::Orders(_) => (7, "orders"),
        Answer::Asks(_) => (8, "asks"),
        Answer::Machines(_) => (9, "machines"),
        Answer::Events { .. } => (10, "events"),
        Answer::Subscribed => (11, "subscribed"),
        Answer::Applied => (12, "applied"),
        Answer::Wip { .. } => (13, "wip"),
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

fn answers() -> Vec<Frame> {
    vec![
        ok(
            1,
            Answer::State {
                state: a_state(),
                at: EventId(411),
            },
            vec![],
        ),
        ok(2, Answer::Row(Some(a_row())), vec![]),
        ok(3, Answer::Rows(vec![a_row()]), vec![]),
        ok(4, Answer::List(None), vec![]),
        ok(5, Answer::Lists(vec![a_list()]), vec![]),
        ok(6, Answer::Seat(Some(a_seat())), vec![]),
        ok(7, Answer::Seats(vec![a_seat()]), vec![]),
        ok(8, Answer::Orders(vec![an_order()]), vec![]),
        ok(9, Answer::Asks(vec![an_ask()]), vec![]),
        ok(10, Answer::Machines(vec![a_machine()]), vec![]),
        ok(
            11,
            Answer::Events {
                events: vec![an_event(401), an_event(402)],
                head: EventId(411),
            },
            vec![],
        ),
        ok(12, Answer::Subscribed, vec![]),
        ok(
            14,
            Answer::Wip {
                state: a_state(),
                at: EventId(411),
            },
            vec![],
        ),
        ok(13, Answer::Applied, vec![412, 413]),
    ]
}

#[test]
fn every_answer_round_trips() {
    table(&answers(), ANSWERS, same, answer_ordinal, ok_name);
}

// ---------------------------------------------------------------- codes

const CODES: usize = 14;

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
        Code::Unknown => (13, "unknown"),
    }
}

fn code_of(f: &Frame) -> Code {
    match f {
        Frame::Reply(Reply {
            outcome: Outcome::Err(e),
            ..
        }) => e.code,
        Frame::Refused(r) => r.code,
        f => panic!("no code: {f:?}"),
    }
}

fn code_name(v: &Value) -> String {
    s(if v["type"] == "refused" {
        &v["code"]
    } else {
        &v["err"]["code"]
    })
}

/// One error of every code: the contract's eight carry their refusal whole,
/// and the three a hello is refused with are also sent as `refused`.
fn errors() -> Vec<Frame> {
    let subject = Subject::Row(row_id());
    vec![
        err(
            1,
            Code::Stale,
            Some(Refusal::Stale {
                lease: fence().lease,
                current: Token(4),
            }),
        ),
        err(
            2,
            Code::TooOld,
            Some(Refusal::TooOld {
                oldest: EventId(100),
            }),
        ),
        err(
            3,
            Code::Conflict,
            Some(Refusal::Conflict {
                current: wsp_store_contract::Rev("r9".into()),
            }),
        ),
        err(
            4,
            Code::Held,
            Some(Refusal::Held {
                lease: fence().lease,
                holder: Principal::Seat(SeatId("cpd-1".into())),
            }),
        ),
        err(
            5,
            Code::NotFound,
            Some(Refusal::NotFound {
                subject: subject.clone(),
            }),
        ),
        err(
            6,
            Code::Illegal,
            Some(Refusal::Illegal {
                subject,
                state: "done".into(),
                verb: "row.claim".into(),
            }),
        ),
        err(7, Code::Forbidden, Some(Refusal::Forbidden { by: ed() })),
        err(
            8,
            Code::Invalid,
            Some(Refusal::Invalid {
                reason: "no key".into(),
            }),
        ),
        err(9, Code::Malformed, None),
        Frame::Refused(Refused {
            code: Code::Malformed,
            text: "the first frame is hello".into(),
        }),
        err(10, Code::Unauthorised, None),
        Frame::Refused(Refused {
            code: Code::Unauthorised,
            text: String::new(),
        }),
        err(11, Code::Protocol, None),
        Frame::Refused(Refused {
            code: Code::Protocol,
            text: "speaks 1".into(),
        }),
        err(12, Code::Unsupported, None),
        err(13, Code::Backend, None),
        err(14, Code::Unknown, None),
    ]
}

#[test]
fn every_error_code_round_trips() {
    table(
        &errors(),
        CODES,
        same,
        |f| code_ordinal(code_of(f)),
        code_name,
    );
}

/// The first eight codes are spelled as the contract's refusal codes, so a
/// server maps a refusal to its code by its tag; each error above that
/// carries a refusal carries the one of its own code.
#[test]
fn the_contract_codes_are_its_refusals_codes() {
    for f in errors() {
        let Frame::Reply(Reply {
            outcome:
                Outcome::Err(Failure {
                    code,
                    refusal: Some(r),
                    ..
                }),
            ..
        }) = &f
        else {
            continue;
        };
        let tag = serde_json::to_value(r).unwrap()["code"].clone();
        assert_eq!(tag, json!(code_ordinal(*code).1), "{f:?}");
        let back: Code = serde_json::from_value(tag).unwrap();
        assert_eq!(back, *code);
    }
}

// ---------------------------------------------------------------- feed

const FEEDS: usize = 3;

fn feed_ordinal(f: &Frame) -> (usize, &'static str) {
    let Frame::Feed(feed) = f else {
        panic!("not a feed frame: {f:?}")
    };
    match feed {
        Feed::State { .. } => (0, "state"),
        Feed::Event(_) => (1, "event"),
        Feed::Beat { .. } => (2, "beat"),
    }
}

fn feeds() -> Vec<Frame> {
    vec![
        Frame::Feed(Feed::State {
            state: a_state(),
            at: EventId(411),
        }),
        Frame::Feed(Feed::Event(an_event(412))),
        Frame::Feed(Feed::Beat {
            beat: EventId(412),
            at: At(1_763_400_010_000),
        }),
    ]
}

#[test]
fn every_feed_frame_round_trips() {
    table(&feeds(), FEEDS, same, feed_ordinal, |v| {
        let mut keys: Vec<&String> = v.as_object().unwrap().keys().collect();
        keys.retain(|k| *k != "type");
        assert_eq!(keys.len(), 1, "{v}");
        keys[0].clone()
    });
}

fn every_frame() -> Vec<Frame> {
    let mut all = frames();
    all.extend(requests());
    all.extend(answers());
    all.extend(errors());
    all.extend(feeds());
    all
}

// ---------------------------------------------------------------- spelling

/// The wire spelling, pinned: a rename in the types is a protocol change and
/// must show here.
#[test]
fn wire_spelling_is_pinned() {
    let claim = requests()
        .into_iter()
        .find(|f| matches!(call_of(f), Call::Command(Command::RowClaim { .. })))
        .unwrap();
    assert_eq!(
        serde_json::to_value(&claim).unwrap(),
        json!({
            "type": "request",
            "v": 1,
            "id": 16,
            "key": "k-16",
            "by": "seat:cpd-3698",
            "verb": "row.claim",
            "args": {
                "row": "wsp-071",
                "seat": "cpd-3698",
                "fence": {"lease": "claim:wsp-071", "token": 3}
            }
        })
    );

    let rows = serde_json::to_value(&requests()[2]).unwrap();
    assert_eq!(rows["verb"], "rows");
    assert!(rows.get("key").is_none(), "a read carries no key");
    assert_eq!(rows["args"]["states"], json!(["doing", "review"]));

    assert_eq!(
        serde_json::to_value(&requests()[0]).unwrap()["args"],
        json!({}),
        "a verb with no arguments sends {{}}"
    );

    assert_eq!(
        serde_json::to_value(answers().last().unwrap()).unwrap(),
        json!({"type": "reply", "id": 13, "ok": "applied", "events": [412, 413]})
    );
    assert_eq!(
        serde_json::to_value(&errors()[1]).unwrap(),
        json!({
            "type": "reply",
            "id": 2,
            "err": {"code": "too_old", "text": "TooOld", "refusal": {"code": "too_old", "oldest": 100}},
            "events": []
        })
    );
    assert_eq!(
        serde_json::to_value(&answers()[1]).unwrap()["ok"]["row"]["id"],
        "wsp-071"
    );

    assert_eq!(
        serde_json::to_value(hello()).unwrap(),
        json!({"type": "hello", "token": "s3cret", "by": "person:ed", "client": "wsp 0.1.0", "protocol": 1})
    );
    assert_eq!(
        serde_json::to_value(&frames()[1]).unwrap(),
        json!({"type": "welcome", "server": "main", "protocol": 1, "head": 411})
    );
    assert_eq!(
        serde_json::to_value(&frames()[2]).unwrap(),
        json!({"type": "refused", "code": "unauthorised", "text": "wrong token"})
    );
    assert_eq!(
        serde_json::to_value(&feeds()[2]).unwrap(),
        json!({"type": "feed", "beat": {"beat": 412, "at": 1_763_400_010_000u64}})
    );
    assert_eq!(
        serde_json::to_value(&feeds()[1]).unwrap()["event"]["kind"],
        "mirror.put"
    );

    assert_eq!(
        serde_json::to_value(Listen {
            addrs: vec!["127.0.0.1:7412".into(), "100.64.0.1:7412".into()]
        })
        .unwrap(),
        json!({"addrs": ["127.0.0.1:7412", "100.64.0.1:7412"]})
    );
    assert_eq!(
        (TOKEN_FILE, LISTEN_FILE, TOKEN_MODE),
        ("api.token", "api.json", 0o600)
    );
}

// ---------------------------------------------------------------- other peers

fn decode(line: &str) -> Frame {
    serde_json::from_str(line).unwrap_or_else(|e| panic!("{line}: {e}"))
}

/// A verb with no arguments decodes with `args` left out, `{}` or `null`. A
/// verb whose arguments all have defaults (a filter, `subscribe`) takes `{}`;
/// without `args` it is no verb this build reads, and the server answers it
/// `malformed`.
#[test]
fn a_verb_without_arguments_decodes_either_way() {
    let line = |verb: &str, args: &str| {
        format!(r#"{{"type":"request","v":1,"id":1,"by":"person:ed","verb":"{verb}"{args}}}"#)
    };
    for args in [
        "",
        r#","args":{}"#,
        r#","args":null"#,
        r#","args":{"newer":1}"#,
    ] {
        for (verb, want) in [
            ("state", Verb::State(NoArgs {})),
            ("lists", Verb::Lists(NoArgs {})),
            ("machines", Verb::Machines(NoArgs {})),
        ] {
            let l = line(verb, args);
            assert_eq!(call_of(&decode(&l)), &Call::Verb(want), "{l}");
        }
    }
    for (verb, want) in [
        ("rows", Verb::Rows(RowFilter::default())),
        ("seats", Verb::Seats(SeatFilter::default())),
        ("subscribe", Verb::Subscribe(Subscribe::default())),
        (
            "orders",
            Verb::Orders {
                seat: None,
                states: vec![],
            },
        ),
        (
            "asks",
            Verb::Asks {
                kinds: vec![],
                states: vec![],
            },
        ),
    ] {
        let l = line(verb, r#","args":{}"#);
        assert_eq!(call_of(&decode(&l)), &Call::Verb(want), "{l}");
        let l = line(verb, "");
        assert!(
            matches!(call_of(&decode(&l)), Call::Unknown { verb: v, .. } if v == verb),
            "{l}"
        );
    }
}

/// A verb this build does not know, and a known verb whose `args` do not fit,
/// still decode as a request, so the server answers its `id`.
#[test]
fn an_unknown_verb_or_bad_args_keeps_the_request_id() {
    let f = decode(
        r#"{"type":"request","v":1,"id":9,"by":"person:ed","verb":"row.teleport","args":{"x":1}}"#,
    );
    let Frame::Request(r) = &f else { panic!() };
    assert_eq!(r.id, ReqId(9));
    assert_eq!(
        r.call,
        Call::Unknown {
            verb: "row.teleport".into(),
            args: json!({"x": 1})
        }
    );

    for (verb, args) in [
        ("row", json!({"rows": 1})),
        ("row.claim", json!({"row": "wsp-1"})),
    ] {
        let line = json!({"type":"request","v":1,"id":10,"by":"person:ed","verb":verb,"args":args})
            .to_string();
        let Frame::Request(r) = decode(&line) else {
            panic!()
        };
        assert_eq!(r.id, ReqId(10));
        match &r.call {
            Call::Unknown { verb: v, .. } => assert!(
                READ_VERBS.contains(&v.as_str()) || COMMAND_VERBS.contains(&v.as_str()),
                "{v} is known, so the server answers malformed"
            ),
            c => panic!("{line} decoded as {c:?}"),
        }
    }
}

/// An older peer leaves out what is optional; a newer one sends fields and
/// codes this build does not know. Both still decode. A frame type this build
/// does not know does not.
#[test]
fn peers_of_other_versions_decode() {
    let older = decode(r#"{"type":"reply","id":3,"ok":{"rows":[]}}"#);
    assert_eq!(older, ok(3, Answer::Rows(vec![]), vec![]));

    let newer = decode(
        r#"{"type":"reply","id":4,"err":{"code":"on_fire","text":"x","more":1},"events":[],"extra":true}"#,
    );
    let Frame::Reply(Reply {
        outcome: Outcome::Err(e),
        ..
    }) = newer
    else {
        panic!()
    };
    assert_eq!(e.code, Code::Unknown);

    let refused = decode(r#"{"type":"refused","code":"tired"}"#);
    assert_eq!(
        refused,
        Frame::Refused(Refused {
            code: Code::Unknown,
            text: String::new()
        })
    );

    let hello = decode(
        r#"{"type":"hello","token":"t","by":"seat:cpd-1","client":"x","protocol":2,"caps":[]}"#,
    );
    assert!(matches!(hello, Frame::Hello(Hello { protocol: 2, .. })));

    assert!(serde_json::from_str::<Frame>(r#"{"type":"teleport","to":"moon"}"#).is_err());
    assert!(
        serde_json::from_str::<Frame>(r#"{"type":"reply","id":1,"events":[]}"#).is_err(),
        "a reply is ok or err"
    );
}

#[test]
fn the_token_is_not_printed() {
    let shown = format!("{:?}", hello());
    assert!(!shown.contains("s3cret"), "{shown}");
    assert!(shown.contains("ApiToken(..)"));
}

// ---------------------------------------------------------------- principal

fn every_principal() -> Vec<(Principal, &'static str)> {
    vec![
        (Principal::Person("ed".into()), "person:ed"),
        (Principal::Seat(SeatId("cpd-3698".into())), "seat:cpd-3698"),
        (Principal::Manager(MachineName("mac".into())), "manager:mac"),
        (
            Principal::Executor(MachineName("eds-macbook-i5".into())),
            "executor:eds-macbook-i5",
        ),
        (Principal::Wsp("main".into()), "wsp:main"),
    ]
}

/// Held exhaustive: a kind added to the contract fails to compile here.
fn principal_ordinal(p: &Principal) -> usize {
    match p {
        Principal::Person(_) => 0,
        Principal::Seat(_) => 1,
        Principal::Manager(_) => 2,
        Principal::Executor(_) => 3,
        Principal::Wsp(_) => 4,
    }
}

#[test]
fn every_principal_kind_round_trips_in_its_text_form() {
    let mut seen = BTreeSet::new();
    for (p, text) in every_principal() {
        assert_eq!(principal::text(&p), text);
        assert_eq!(principal::parse(text), Ok(p.clone()));
        seen.insert(principal_ordinal(&p));
        assert_eq!(
            principal::KINDS[principal_ordinal(&p)],
            text.split(':').next().unwrap()
        );
    }
    assert_eq!(seen.len(), principal::KINDS.len());

    // A name may hold a `:` or a space; the kind ends at the first `:`.
    for p in [
        Principal::Wsp("a:b".into()),
        Principal::Person("Ed Jensen".into()),
        Principal::Manager(MachineName("[::1]".into())),
    ] {
        assert_eq!(principal::parse(&principal::text(&p)), Ok(p));
    }
}

#[test]
fn a_malformed_principal_is_an_error_not_a_panic() {
    for bad in [
        "",
        "ed",
        ":ed",
        "person:",
        "robot:x",
        "PERSON:ed",
        " person:ed",
        "seat",
    ] {
        let e = principal::parse(bad).unwrap_err();
        assert_eq!(e.text, bad);
        assert!(!e.to_string().is_empty());
    }
    let line = r#"{"type":"request","v":1,"id":1,"by":"robot:x","verb":"state"}"#;
    let e = serde_json::from_str::<Frame>(line).unwrap_err();
    assert!(e.to_string().contains("not a principal"), "{e}");
    let line = r#"{"type":"hello","token":"t","by":"ed","client":"x","protocol":1}"#;
    assert!(serde_json::from_str::<Frame>(line).is_err());
}

// ---------------------------------------------------------------- framing

#[test]
fn the_memory_pair_carries_every_frame() {
    let (mut a, mut b) = pair();
    for f in every_frame() {
        a.send(&f).unwrap();
        assert_eq!(b.recv(Duration::from_secs(1)).unwrap(), Some(f.clone()));
        b.send(&f).unwrap();
        assert_eq!(a.recv(Duration::from_secs(1)).unwrap(), Some(f));
    }
}

/// One frame per line: a raw newline inside a line or a line that is no frame
/// is `Malformed`, and the connection stays usable; a quiet wait is `None`.
#[test]
fn the_memory_pair_quiet_malformed_and_closed() {
    let (mut a, mut b) = pair();
    assert_eq!(
        b.recv(Duration::from_millis(10)).unwrap(),
        None,
        "a quiet wait is not an error"
    );

    let good = request(1, Call::Verb(Verb::Machines(NoArgs {})));
    let line = serde_json::to_string(&good).unwrap();

    a.send_line(&format!("{}\n{}\n", &line[..20], &line[20..]))
        .unwrap();
    assert!(
        matches!(b.recv(Duration::from_secs(1)), Err(ConnError::Malformed(_))),
        "a raw newline inside a frame"
    );
    a.send_line(&line).unwrap();
    assert!(
        matches!(b.recv(Duration::from_secs(1)), Err(ConnError::Malformed(_))),
        "no newline"
    );
    a.send_line(&format!("{line}\n{line}\n")).unwrap();
    assert!(
        matches!(b.recv(Duration::from_secs(1)), Err(ConnError::Malformed(_))),
        "two frames on one line"
    );
    a.send_line("{\"type\":\"seats\"}\n").unwrap();
    assert!(
        matches!(b.recv(Duration::from_secs(1)), Err(ConnError::Malformed(_))),
        "a line that is no frame"
    );
    a.send_line("not json\n").unwrap();
    assert!(matches!(
        b.recv(Duration::from_secs(1)),
        Err(ConnError::Malformed(_))
    ));

    a.send(&good).unwrap();
    assert_eq!(
        b.recv(Duration::from_secs(1)).unwrap(),
        Some(good.clone()),
        "still usable after a bad line"
    );

    drop(a);
    assert_eq!(b.recv(Duration::from_secs(1)), Err(ConnError::Closed));
    assert_eq!(b.send(&good), Err(ConnError::Closed));
}

/// A line over the 16 MB cap is not decoded: the carrier fails it as `Io`.
#[test]
fn a_line_over_the_cap_is_not_read() {
    assert_eq!(MAX_LINE, 16 * 1024 * 1024);
    let (mut a, mut b) = pair();
    let mut long = String::with_capacity(MAX_LINE + 2);
    long.push_str("{\"type\":\"refused\",\"code\":\"x\",\"text\":\"");
    long.extend(std::iter::repeat_n('a', MAX_LINE));
    long.push_str("\"}\n");
    a.send_line(&long).unwrap();
    assert!(matches!(
        b.recv(Duration::from_secs(5)),
        Err(ConnError::Io(_))
    ));

    let just = Frame::Refused(Refused {
        code: Code::Malformed,
        text: "a".repeat(1024 * 1024),
    });
    a.send(&just).unwrap();
    assert_eq!(b.recv(Duration::from_secs(5)).unwrap(), Some(just));
}

/// An event a feed carries is the contract's `Event`, payload and all.
#[test]
fn a_feed_event_carries_the_contracts_event_whole() {
    let Frame::Feed(Feed::Event(e)) = &feeds()[1] else {
        panic!()
    };
    let entity: Entity = serde_json::from_value(e.payload.clone()).unwrap();
    assert_eq!(entity, Entity::Seat(a_seat()));
}
