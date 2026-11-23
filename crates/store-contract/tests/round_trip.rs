//! Every command, event kind, entity state and refusal round-trips through
//! serde, table-driven. Each table's coverage is held by an exhaustive match
//! (`ordinal`), so a new variant does not compile until it has an arm; give it
//! the next ordinal, bump the table's count, and add its frame to the table.
//! Then a trivial type implements the whole trait family and a `Domain`, which
//! shows the family can be implemented, and as `dyn` (object-safe).

use std::cell::RefCell;
use std::collections::{BTreeMap, BTreeSet};
use std::fmt::Debug;
use std::time::Duration;

use serde::{de::DeserializeOwned, Serialize};
use serde_json::{json, Value};
use wsp_store_contract::*;

fn trip<T: Serialize + DeserializeOwned + PartialEq + Debug>(v: &T) -> Value {
    let text = serde_json::to_string(v).unwrap();
    let back: T = serde_json::from_str(&text).unwrap();
    assert_eq!(&back, v, "{text}");
    serde_json::from_str(&text).unwrap()
}

/// Checks a table: every frame round-trips, its wire name is the ordinal's,
/// and every ordinal below `count` is in the table.
fn table<T: Serialize + DeserializeOwned + PartialEq + Debug>(
    frames: &[T],
    count: usize,
    ordinal: fn(&T) -> (usize, &'static str),
    name: fn(&Value) -> String,
) {
    let mut seen = BTreeSet::new();
    for f in frames {
        let (n, wire) = ordinal(f);
        assert!(
            n < count,
            "{f:?}: ordinal {n} is not below the count {count}; bump it"
        );
        assert_eq!(name(&trip(f)), wire, "{f:?}");
        seen.insert(n);
    }
    let missing: Vec<usize> = (0..count).filter(|n| !seen.contains(n)).collect();
    assert!(
        missing.is_empty(),
        "ordinals with no frame in the table: {missing:?}"
    );
}

fn as_str(v: &Value) -> String {
    v.as_str().unwrap().to_string()
}

fn row() -> RowId {
    RowId("wsp-046".into())
}
fn seat() -> SeatId {
    SeatId("cpd-336".into())
}
fn list() -> ListId {
    ListId("s0-state-core".into())
}
fn fence() -> Fence {
    Fence {
        lease: LeaseName("claim:wsp-046".into()),
        token: Token(7),
    }
}
fn ed() -> Principal {
    Principal::Person("ed".into())
}

const COMMANDS: usize = 25;

fn command(c: &Command) -> (usize, &'static str) {
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
        Command::OrderDeliver { .. } => (20, "order.deliver"),
        Command::OrderDelivered { .. } => (21, "order.delivered"),
        Command::OrderAcked { .. } => (22, "order.acked"),
        Command::OrderRefused { .. } => (23, "order.refused"),
        Command::OrderDone { .. } => (24, "order.done"),
    }
}

fn commands() -> Vec<Command> {
    vec![
        Command::RowAdd {
            project: ProjectId("wsp".into()),
            title: "S0".into(),
            fields: BTreeMap::from([("tier".into(), "opus".into())]),
        },
        Command::RowEdit {
            row: row(),
            title: None,
            fields: BTreeMap::from([("tier".into(), None)]),
        },
        Command::RowClaim {
            row: row(),
            seat: seat(),
            fence: fence(),
        },
        Command::RowReview {
            row: row(),
            commit: "84f81bc".into(),
            fence: fence(),
        },
        Command::RowBlock {
            row: row(),
            block: Block::On {
                reason: "waits".into(),
            },
        },
        Command::RowPark { row: row() },
        Command::RowLand {
            row: row(),
            verdict: Verdict::Holds {
                commit: "84f81bc".into(),
            },
            fence: fence(),
        },
        Command::ListGo { list: list() },
        Command::ListHold {
            list: list(),
            group: Some(2),
            reason: "look".into(),
        },
        Command::ListRelease {
            list: list(),
            group: 2,
            to: ReleaseTo::Barrier,
        },
        Command::SeatSpawn {
            row: Some(row()),
            attempt: 1,
            machine: None,
            kind: "claude".into(),
            model: Some("opus".into()),
        },
        Command::SeatEnd {
            seat: seat(),
            fence: Some(fence()),
        },
        Command::SeatDespawn {
            seat: seat(),
            lost: true,
        },
        Command::OrderSend {
            to: seat(),
            text: "go".into(),
            first: true,
        },
        Command::OrderCancel {
            order: OrderId("ord-1".into()),
        },
        Command::AskRaise {
            kind: AskKind::Flag,
            to: Some(ed()),
            about: Some(Subject::Row(row())),
            text: "?".into(),
        },
        Command::AskAnswer {
            ask: AskId("m-1".into()),
            text: "yes".into(),
        },
        Command::AskAck {
            ask: AskId("m-1".into()),
        },
        Command::MachineSet {
            name: MachineName("mac".into()),
            transport: Some(Transport::Tcp {
                addr: "mac:7070".into(),
            }),
            facts: BTreeMap::new(),
        },
        Command::Mirror {
            put: entities(),
            gone: vec![Subject::Seat(seat()), Subject::Claim(row())],
        },
        Command::OrderDeliver {
            order: OrderId("ord-1".into()),
            fence: Fence {
                lease: LeaseName::deliver(&seat()),
                token: Token(3),
            },
        },
        Command::OrderDelivered {
            order: OrderId("ord-1".into()),
        },
        Command::OrderAcked {
            order: OrderId("ord-1".into()),
            turn_began: At(2_000),
        },
        Command::OrderRefused {
            order: OrderId("ord-1".into()),
            why: "busy".into(),
            counts: false,
        },
        Command::OrderDone {
            order: OrderId("ord-1".into()),
        },
    ]
}

/// `LeaseName::deliver` names `deliver:<seat>` (2.1), on the wire as the bare string.
#[test]
fn the_deliver_lease_is_named_deliver_and_the_seat() {
    let name = LeaseName::deliver(&seat());
    assert_eq!(name, LeaseName("deliver:cpd-336".into()));
    assert_eq!(trip(&name), json!("deliver:cpd-336"));
}

/// `FENCED_VERBS` is exactly the verbs whose `args` carry a `fence` (wsp-065):
/// `commands()` holds every variant (the table above), so a fenced verb added
/// later fails here until the table lists it, and a listed verb that lost its
/// fence fails too.
#[test]
fn fenced_verbs_are_exactly_the_commands_carrying_a_fence() {
    let mut carrying = BTreeSet::new();
    for c in commands() {
        let v = serde_json::to_value(&c).unwrap();
        if v["args"].get("fence").is_some() {
            carrying.insert(as_str(&v["verb"]));
        }
    }
    let listed: BTreeSet<String> = FENCED_VERBS.iter().map(|v| v.to_string()).collect();
    assert_eq!(carrying, listed);
    assert_eq!(STORE_INSTANCE, "store");
}

#[test]
fn every_command_round_trips_under_its_verb() {
    table(&commands(), COMMANDS, command, |v| as_str(&v["verb"]));
    // The other halves of the commands' arguments.
    trip(&Block::Off);
    trip(&Verdict::Blocks {
        reason: "red".into(),
    });
    trip(&ReleaseTo::Running);
    trip(&Applied {
        events: vec![EventId(1), EventId(2)],
    });
}

const EVENT_KINDS: usize = 48;

fn event_kind(k: &EventKind) -> (usize, &'static str) {
    use EventKind::*;
    match k {
        RowAdded => (0, "row.added"),
        RowEdited => (1, "row.edited"),
        RowClaimed => (2, "row.claimed"),
        RowReviewed => (3, "row.reviewed"),
        RowBlocked => (4, "row.blocked"),
        RowUnblocked => (5, "row.unblocked"),
        RowParked => (6, "row.parked"),
        RowSentBack => (7, "row.sent_back"),
        RowLanded => (8, "row.landed"),
        ListStarted => (9, "list.started"),
        ListFinished => (10, "list.finished"),
        GroupRunning => (11, "group.running"),
        GroupBarrier => (12, "group.barrier"),
        GroupPassed => (13, "group.passed"),
        GroupHeld => (14, "group.held"),
        GroupReleased => (15, "group.released"),
        SeatRequested => (16, "seat.requested"),
        SeatSpawning => (17, "seat.spawning"),
        SeatStarted => (18, "seat.started"),
        SeatWorking => (19, "seat.working"),
        SeatIdle => (20, "seat.idle"),
        SeatEnding => (21, "seat.ending"),
        SeatGone => (22, "seat.gone"),
        SeatLost => (23, "seat.lost"),
        SeatReturned => (24, "seat.returned"),
        OrderQueued => (25, "order.queued"),
        OrderTyped => (26, "order.typed"),
        OrderDelivered => (27, "order.delivered"),
        OrderRefused => (28, "order.refused"),
        OrderAcknowledged => (29, "order.acknowledged"),
        OrderDone => (30, "order.done"),
        OrderCancelled => (31, "order.cancelled"),
        OrderFailed => (32, "order.failed"),
        AskRaised => (33, "ask.raised"),
        AskAnswered => (34, "ask.answered"),
        AskAcked => (35, "ask.acked"),
        ClaimTaken => (36, "claim.taken"),
        ClaimReleased => (37, "claim.released"),
        LeaseTaken => (38, "lease.taken"),
        LeaseReleased => (39, "lease.released"),
        RunStarted => (40, "run.started"),
        RunFinished => (41, "run.finished"),
        MachineSet => (42, "machine.set"),
        MachineBeat => (43, "machine.beat"),
        TextSet => (44, "text.set"),
        MirrorPut => (45, "mirror.put"),
        MirrorGone => (46, "mirror.gone"),
        Unknown => (47, "unknown"),
    }
}

#[test]
fn every_event_kind_round_trips_under_its_name() {
    use EventKind::*;
    let kinds = [
        RowAdded,
        RowEdited,
        RowClaimed,
        RowReviewed,
        RowBlocked,
        RowUnblocked,
        RowParked,
        RowSentBack,
        RowLanded,
        ListStarted,
        ListFinished,
        GroupRunning,
        GroupBarrier,
        GroupPassed,
        GroupHeld,
        GroupReleased,
        SeatRequested,
        SeatSpawning,
        SeatStarted,
        SeatWorking,
        SeatIdle,
        SeatEnding,
        SeatGone,
        SeatLost,
        SeatReturned,
        OrderQueued,
        OrderTyped,
        OrderDelivered,
        OrderRefused,
        OrderAcknowledged,
        OrderDone,
        OrderCancelled,
        OrderFailed,
        AskRaised,
        AskAnswered,
        AskAcked,
        ClaimTaken,
        ClaimReleased,
        LeaseTaken,
        LeaseReleased,
        RunStarted,
        RunFinished,
        MachineSet,
        MachineBeat,
        TextSet,
        MirrorPut,
        MirrorGone,
        Unknown,
    ];
    table(&kinds, EVENT_KINDS, event_kind, as_str);
    // A newer writer's kind reads as Unknown.
    let k: EventKind = serde_json::from_value(json!("seat.teleported")).unwrap();
    assert_eq!(k, Unknown);
}

/// One ordinal space over every entity state enum, so one table covers them all.
#[derive(Debug, PartialEq)]
enum AnyState {
    Row(RowState),
    List(ListState),
    Seat(SeatState),
    Order(OrderState),
    AskKind(AskKind),
    Ask(AskState),
    RunKind(RunKind),
    Run(RunState),
    Verdict(RunVerdict),
}

fn entity_state(s: &AnyState) -> (usize, &'static str) {
    use AnyState as A;
    match s {
        A::Row(r) => match r {
            RowState::Todo => (0, "todo"),
            RowState::Doing => (1, "doing"),
            RowState::Review => (2, "review"),
            RowState::SentBack => (3, "sent_back"),
            RowState::Blocked => (4, "blocked"),
            RowState::Parked => (5, "parked"),
            RowState::Done => (6, "done"),
        },
        A::List(l) => match l {
            ListState::Planned => (7, "planned"),
            ListState::Running => (8, "running"),
            ListState::Pending => (9, "pending"),
            ListState::Barrier => (10, "barrier"),
            ListState::Held => (11, "held"),
            ListState::Passed => (12, "passed"),
            ListState::Finished => (13, "finished"),
        },
        A::Seat(s) => match s {
            SeatState::Requested => (14, "requested"),
            SeatState::Spawning => (15, "spawning"),
            SeatState::Started => (16, "started"),
            SeatState::Working => (17, "working"),
            SeatState::Idle => (18, "idle"),
            SeatState::Ending => (19, "ending"),
            SeatState::Gone => (20, "gone"),
            SeatState::Lost => (21, "lost"),
        },
        A::Order(o) => match o {
            OrderState::Queued => (22, "queued"),
            OrderState::Typed => (23, "typed"),
            OrderState::Delivered => (24, "delivered"),
            OrderState::Acknowledged => (25, "acknowledged"),
            OrderState::Done => (26, "done"),
            OrderState::Cancelled => (27, "cancelled"),
            OrderState::Failed => (28, "failed"),
        },
        A::AskKind(k) => match k {
            AskKind::Ask => (29, "ask"),
            AskKind::Note => (30, "note"),
            AskKind::Flag => (31, "flag"),
        },
        A::Ask(a) => match a {
            AskState::Raised => (32, "raised"),
            AskState::Acked => (33, "acked"),
            AskState::Answered => (34, "answered"),
        },
        A::RunKind(k) => match k {
            RunKind::Check => (35, "check"),
            RunKind::Verification => (36, "verification"),
            RunKind::Barrier => (37, "barrier"),
        },
        A::Run(r) => match r {
            RunState::Running => (38, "running"),
            RunState::Finished => (39, "finished"),
            RunState::Abandoned => (40, "abandoned"),
        },
        A::Verdict(v) => match v {
            RunVerdict::Pass => (41, "pass"),
            RunVerdict::Fail => (42, "fail"),
        },
    }
}

const ENTITY_STATES: usize = 43;

/// Round-trips each state through its own type (`"running"` is a list's and a
/// run's), then checks the table's coverage.
#[test]
fn every_entity_state_round_trips_under_its_name() {
    use AnyState as A;
    let mut all = vec![];
    all.extend(
        [
            RowState::Todo,
            RowState::Doing,
            RowState::Review,
            RowState::SentBack,
            RowState::Blocked,
            RowState::Parked,
            RowState::Done,
        ]
        .map(A::Row),
    );
    all.extend(
        [
            ListState::Planned,
            ListState::Running,
            ListState::Pending,
            ListState::Barrier,
            ListState::Held,
            ListState::Passed,
            ListState::Finished,
        ]
        .map(A::List),
    );
    all.extend(
        [
            SeatState::Requested,
            SeatState::Spawning,
            SeatState::Started,
            SeatState::Working,
            SeatState::Idle,
            SeatState::Ending,
            SeatState::Gone,
            SeatState::Lost,
        ]
        .map(A::Seat),
    );
    all.extend(
        [
            OrderState::Queued,
            OrderState::Typed,
            OrderState::Delivered,
            OrderState::Acknowledged,
            OrderState::Done,
            OrderState::Cancelled,
            OrderState::Failed,
        ]
        .map(A::Order),
    );
    all.extend([AskKind::Ask, AskKind::Note, AskKind::Flag].map(A::AskKind));
    all.extend([AskState::Raised, AskState::Acked, AskState::Answered].map(A::Ask));
    all.extend([RunKind::Check, RunKind::Verification, RunKind::Barrier].map(A::RunKind));
    all.extend([RunState::Running, RunState::Finished, RunState::Abandoned].map(A::Run));
    all.extend([RunVerdict::Pass, RunVerdict::Fail].map(A::Verdict));
    let mut seen = BTreeSet::new();
    for s in &all {
        let (n, wire) = entity_state(s);
        assert!(n < ENTITY_STATES, "{s:?}: bump ENTITY_STATES");
        let v = match s {
            A::Row(x) => trip(x),
            A::List(x) => trip(x),
            A::Seat(x) => trip(x),
            A::Order(x) => trip(x),
            A::AskKind(x) => trip(x),
            A::Ask(x) => trip(x),
            A::RunKind(x) => trip(x),
            A::Run(x) => trip(x),
            A::Verdict(x) => trip(x),
        };
        assert_eq!(as_str(&v), wire, "{s:?}");
        seen.insert(n);
    }
    assert_eq!(
        seen.len(),
        ENTITY_STATES,
        "a state has no frame in the table"
    );
}

const REFUSALS: usize = 9;

fn refusal(r: &Refusal) -> (usize, &'static str) {
    match r {
        Refusal::Stale { .. } => (0, "stale"),
        Refusal::TooOld { .. } => (1, "too_old"),
        Refusal::Conflict { .. } => (2, "conflict"),
        Refusal::Held { .. } => (3, "held"),
        Refusal::NotFound { .. } => (4, "not_found"),
        Refusal::Illegal { .. } => (5, "illegal"),
        Refusal::Forbidden { .. } => (6, "forbidden"),
        Refusal::Invalid { .. } => (7, "invalid"),
        Refusal::Unknown => (8, "unknown"),
    }
}

#[test]
fn every_refusal_round_trips_under_its_code() {
    let all = [
        Refusal::Stale {
            lease: fence().lease,
            current: Token(8),
        },
        Refusal::TooOld {
            oldest: EventId(100),
        },
        Refusal::Conflict {
            current: Rev("r2".into()),
        },
        Refusal::Held {
            lease: LeaseName("tick".into()),
            holder: Principal::Wsp("main".into()),
        },
        Refusal::NotFound {
            subject: Subject::Seat(seat()),
        },
        Refusal::Illegal {
            subject: Subject::Row(row()),
            state: "done".into(),
            verb: "row.claim".into(),
        },
        Refusal::Forbidden {
            by: Principal::Seat(seat()),
        },
        Refusal::Invalid {
            reason: "no seat".into(),
        },
        Refusal::Unknown,
    ];
    table(&all, REFUSALS, refusal, |v| as_str(&v["code"]));
    let r: Refusal = serde_json::from_value(json!({"code": "rate_limited", "after": 3})).unwrap();
    assert_eq!(r, Refusal::Unknown, "a newer peer's code reads as Unknown");
    trip(&Error::Refused(Refusal::TooOld { oldest: EventId(1) }));
    trip(&Error::Backend {
        reason: "disk".into(),
    });
}

fn state() -> State {
    let group = GroupId { list: list(), n: 2 };
    let mut s = State::default();
    s.rows.insert(
        row(),
        Row {
            id: row(),
            project: ProjectId("wsp".into()),
            title: "S0".into(),
            state: RowState::Blocked,
            before_block: Some(RowState::Doing),
            seat: Some(seat()),
            group: Some(group.clone()),
            commit: None,
            fields: BTreeMap::new(),
        },
    );
    s.lists.insert(
        list(),
        List {
            id: list(),
            state: ListState::Running,
            groups: vec![Group {
                id: group,
                state: ListState::Barrier,
                members: vec![row()],
                attempt: 1,
                fields: BTreeMap::from([("stop".into(), "after".into())]),
            }],
            fields: BTreeMap::from([("caption".into(), "S0".into())]),
        },
    );
    s.seats.insert(
        seat(),
        Seat {
            id: seat(),
            machine: MachineName("mac".into()),
            kind: "claude".into(),
            model: Some("opus".into()),
            state: SeatState::Lost,
            before_lost: Some(SeatState::Working),
            row: Some(row()),
            attempt: 1,
            pid: Some(4242),
            fields: BTreeMap::from([
                ("role".into(), "governor".into()),
                ("governs".into(), "wsp-046".into()),
            ]),
        },
    );
    s.claims.insert(
        row(),
        Claim {
            row: row(),
            seat: seat(),
            token: Token(7),
            expires: At(1_000),
        },
    );
    s.orders.insert(
        OrderId("ord-1".into()),
        Order {
            id: OrderId("ord-1".into()),
            from: ed(),
            to: seat(),
            text: "go".into(),
            state: OrderState::Typed,
            attempts: 1,
            first: true,
            why: Some("busy".into()),
        },
    );
    s.asks.insert(
        AskId("m-1".into()),
        Ask {
            id: AskId("m-1".into()),
            kind: AskKind::Ask,
            from: Principal::Seat(seat()),
            to: Some(ed()),
            about: Some(Subject::Run(RunKey {
                row: row(),
                attempt: 1,
                kind: RunKind::Check,
            })),
            text: "?".into(),
            state: AskState::Answered,
            answer: Some("yes".into()),
            fields: BTreeMap::from([("person".into(), "yes".into())]),
        },
    );
    s.runs.push(Run {
        key: RunKey {
            row: row(),
            attempt: 1,
            kind: RunKind::Verification,
        },
        state: RunState::Finished,
        verdict: Some(RunVerdict::Pass),
        log: Some("verdicts/wsp-046".into()),
    });
    s.machines.insert(
        MachineName("mac".into()),
        Machine {
            name: MachineName("mac".into()),
            transport: Transport::Local,
            beat: Some(At(900)),
            facts: BTreeMap::from([("load".into(), "4".into())]),
        },
    );
    s.leases.insert(
        LeaseName("tick".into()),
        Lease {
            name: LeaseName("tick".into()),
            holder: Principal::Wsp("main".into()),
            token: Token(3),
            expires: At(2_000),
        },
    );
    s
}

const ENTITIES: usize = 7;

fn entity(e: &Entity) -> (usize, &'static str) {
    match e {
        Entity::Row(_) => (0, "row"),
        Entity::List(_) => (1, "list"),
        Entity::Seat(_) => (2, "seat"),
        Entity::Claim(_) => (3, "claim"),
        Entity::Order(_) => (4, "order"),
        Entity::Ask(_) => (5, "ask"),
        Entity::Machine(_) => (6, "machine"),
    }
}

/// One of every entity `mirror` puts, taken from `state()`.
fn entities() -> Vec<Entity> {
    let s = state();
    let mut out = vec![];
    out.extend(s.rows.into_values().map(Entity::Row));
    out.extend(s.lists.into_values().map(Entity::List));
    out.extend(s.seats.into_values().map(Entity::Seat));
    out.extend(s.claims.into_values().map(Entity::Claim));
    out.extend(s.orders.into_values().map(Entity::Order));
    out.extend(s.asks.into_values().map(Entity::Ask));
    out.extend(s.machines.into_values().map(Entity::Machine));
    out
}

/// Each entity round-trips under its tag, in serde's derived form (`{"seat": {...}}`).
#[test]
fn every_entity_round_trips_under_its_tag() {
    table(&entities(), ENTITIES, entity, |v| {
        let o = v.as_object().unwrap();
        assert_eq!(o.len(), 1, "{v}");
        o.keys().next().unwrap().clone()
    });
    // `mirror`'s arguments default, so `{}` is a mirror of nothing.
    let none: Command = serde_json::from_value(json!({"verb": "mirror", "args": {}})).unwrap();
    assert_eq!(none, Command::Mirror { put: vec![], gone: vec![] });
    // A mirror event: the entity under its subject, and `null` for a gone one.
    let put = trip(&Draft {
        kind: EventKind::MirrorPut,
        subject: Subject::Seat(seat()),
        payload: serde_json::to_value(&entities()[2]).unwrap(),
    });
    assert_eq!(put["payload"]["seat"]["fields"]["role"], "governor");
    trip(&Draft {
        kind: EventKind::MirrorGone,
        subject: Subject::Seat(seat()),
        payload: Value::Null,
    });
}

/// Frames written before wsp-070 (a `Seat`, `List`, `Group` or `Ask` with no
/// `fields`) still decode, with no fields.
#[test]
fn frames_from_before_the_fields_maps_still_decode() {
    let seat: Seat = serde_json::from_value(json!({
        "id": "cpd-336", "machine": "mac", "kind": "claude", "model": null,
        "state": "idle", "before_lost": null, "row": "wsp-046", "attempt": 1, "pid": 4242
    }))
    .unwrap();
    assert!(seat.fields.is_empty());
    assert_eq!(seat.state, SeatState::Idle);
    let list: List = serde_json::from_value(json!({
        "id": "s0-state-core", "state": "running",
        "groups": [{"id": {"list": "s0-state-core", "n": 1}, "state": "running", "members": ["wsp-046"], "attempt": 0}]
    }))
    .unwrap();
    assert!(list.fields.is_empty() && list.groups[0].fields.is_empty());
    let ask: Ask = serde_json::from_value(json!({
        "id": "m-1", "kind": "note", "from": {"person": "ed"}, "text": "hi", "state": "raised"
    }))
    .unwrap();
    assert!(ask.fields.is_empty());
}

/// An `order.send` frame and an `Order` written before wsp-117 (no `first`, no
/// `why`) still decode: not the first order, and no refusal's words.
#[test]
fn orders_from_before_first_and_why_still_decode() {
    let send: Command = serde_json::from_value(json!({
        "verb": "order.send", "args": {"to": "cpd-336", "text": "go"}
    }))
    .unwrap();
    assert_eq!(
        send,
        Command::OrderSend {
            to: seat(),
            text: "go".into(),
            first: false,
        }
    );
    let order: Order = serde_json::from_value(json!({
        "id": "ord-1", "from": {"person": "ed"}, "to": "cpd-336", "text": "go",
        "state": "queued", "attempts": 0
    }))
    .unwrap();
    assert!(!order.first);
    assert_eq!(order.why, None);
    assert_eq!(order.state, OrderState::Queued);
}

#[test]
fn the_whole_state_events_and_feed_round_trip() {
    let s = state();
    trip(&s);
    let principals = [
        ed(),
        Principal::Seat(seat()),
        Principal::Manager(MachineName("mac".into())),
        Principal::Executor(MachineName("i5".into())),
        Principal::Wsp("main".into()),
    ];
    for p in &principals {
        trip(p);
    }
    let subjects = [
        Subject::Project(ProjectId("wsp".into())),
        Subject::Row(row()),
        Subject::List(list()),
        Subject::Group(GroupId { list: list(), n: 1 }),
        Subject::Seat(seat()),
        Subject::Claim(row()),
        Subject::Order(OrderId("ord-1".into())),
        Subject::Ask(AskId("m-1".into())),
        Subject::Run(RunKey {
            row: row(),
            attempt: 0,
            kind: RunKind::Barrier,
        }),
        Subject::Machine(MachineName("mac".into())),
        Subject::Lease(LeaseName("tick".into())),
    ];
    for sub in &subjects {
        trip(sub);
    }
    let e = Event {
        id: EventId(12),
        at: At(5),
        principal: Principal::Executor(MachineName("mac".into())),
        kind: EventKind::OrderAcknowledged,
        subject: Subject::Order(OrderId("ord-1".into())),
        payload: json!({"turn": 3}),
    };
    let v = trip(&e);
    assert_eq!(v["kind"], "order.acknowledged");
    trip(&Draft {
        kind: EventKind::SeatStarted,
        subject: Subject::Seat(seat()),
        payload: Value::Null,
    });
    trip(&Feed::State {
        state: s,
        at: EventId(12),
    });
    trip(&Feed::Event(e));
    trip(&Feed::Beat {
        beat: EventId(12),
        at: At(6),
    });
    trip(&Subscribe {
        from: Some(EventId(12)),
    });
    let absent: Subscribe = serde_json::from_value(json!({})).unwrap();
    assert_eq!(absent.from, None);
    trip(&RowFilter {
        project: None,
        states: vec![RowState::Todo],
        list: Some(list()),
        seat: None,
    });
    trip(&SeatFilter {
        machine: Some(MachineName("mac".into())),
        states: vec![SeatState::Idle],
        row: None,
    });
}

/// The trivial implementation: answers every read from one `State`, refuses
/// every write. Enough to show the family can be implemented, not a backend.
struct Nothing {
    state: RefCell<State>,
}

fn no<T>() -> Result<T> {
    Err(Error::Refused(Refusal::Invalid {
        reason: "read-only".into(),
    }))
}

impl Reads for Nothing {
    fn row(&self, id: &RowId) -> Result<Option<Row>> {
        Ok(self.state.borrow().rows.get(id).cloned())
    }
    fn rows(&self, _: &RowFilter) -> Result<Vec<Row>> {
        Ok(self.state.borrow().rows.values().cloned().collect())
    }
    fn seat(&self, id: &SeatId) -> Result<Option<Seat>> {
        Ok(self.state.borrow().seats.get(id).cloned())
    }
    fn seats(&self, _: &SeatFilter) -> Result<Vec<Seat>> {
        Ok(self.state.borrow().seats.values().cloned().collect())
    }
    fn list(&self, id: &ListId) -> Result<Option<List>> {
        Ok(self.state.borrow().lists.get(id).cloned())
    }
    fn orders_for(&self, _: &SeatId, _: &[OrderState]) -> Result<Vec<Order>> {
        Ok(vec![])
    }
    fn claim(&self, row: &RowId) -> Result<Option<Claim>> {
        Ok(self.state.borrow().claims.get(row).cloned())
    }
    fn lease(&self, name: &LeaseName) -> Result<Option<Lease>> {
        Ok(self.state.borrow().leases.get(name).cloned())
    }
    fn machine(&self, name: &MachineName) -> Result<Option<Machine>> {
        Ok(self.state.borrow().machines.get(name).cloned())
    }
    fn snapshot(&self) -> Result<(State, EventId)> {
        Ok((self.state.borrow().clone(), EventId(0)))
    }
    fn wip_snapshot(&self) -> Result<(State, EventId)> {
        self.snapshot()
    }
}

impl Events for Nothing {
    fn events_after(&self, _: EventId, _: usize) -> Result<Vec<Event>> {
        Ok(vec![])
    }
    fn head(&self) -> Result<EventId> {
        Ok(EventId(0))
    }
    fn wait_for(&self, _: EventId, _: Duration) -> Result<EventId> {
        Ok(EventId(0))
    }
}

impl Commands for Nothing {
    fn apply(&self, _: Command, _: IdempotencyKey, _: Principal) -> Result<Applied> {
        no()
    }
}

impl Leases for Nothing {
    fn take(&self, _: &LeaseName, _: &Principal, _: Duration) -> Result<Option<Token>> {
        Ok(None)
    }
    fn renew(&self, _: &LeaseName, _: Token, _: Duration) -> Result<bool> {
        Ok(false)
    }
    fn release(&self, _: &LeaseName, _: Token) -> Result<()> {
        Ok(())
    }
}

impl Claims for Nothing {
    fn claim(&self, _: &RowId, _: &SeatId, _: Duration, _: Option<&Fence>) -> Result<Token> {
        no()
    }
    fn release(&self, _: &RowId, _: Token) -> Result<()> {
        Ok(())
    }
}

impl Texts for Nothing {
    fn text(&self, _: &RowId, _: &str) -> Result<(String, Rev)> {
        Ok((String::new(), Rev("0".into())))
    }
    fn set_text(&self, _: &RowId, _: &str, _: String, expect: Rev) -> Result<Rev> {
        Err(Error::Refused(Refusal::Conflict { current: expect }))
    }
}

/// A domain that refuses every command and ignores every event.
struct Inert;

impl Domain for Inert {
    fn decide(
        &self,
        _: &State,
        cmd: &Command,
        _: &Principal,
    ) -> std::result::Result<Vec<Draft>, Refusal> {
        Err(Refusal::Invalid {
            reason: format!("{cmd:?}"),
        })
    }
    fn evolve(&self, state: State, _: &Event) -> State {
        state
    }
}

#[test]
fn a_trivial_type_implements_the_family_and_as_dyn() {
    let n = Nothing {
        state: RefCell::new(state()),
    };
    let store: &dyn Store = &n;
    assert_eq!(store.row(&row()).unwrap().unwrap().state, RowState::Blocked);
    assert_eq!(store.head().unwrap(), EventId(0));
    assert!(store
        .apply(
            Command::RowPark { row: row() },
            IdempotencyKey("k".into()),
            ed()
        )
        .is_err());
    assert!(Reads::claim(store, &row()).unwrap().is_some());
    Leases::release(store, &LeaseName("tick".into()), Token(3)).unwrap();
    let domain: &dyn Domain = &Inert;
    assert!(domain
        .decide(&state(), &Command::ListGo { list: list() }, &ed())
        .is_err());
    assert_eq!(
        domain.evolve(
            State::default(),
            &Event {
                id: EventId(1),
                at: At(1),
                principal: ed(),
                kind: EventKind::Unknown,
                subject: Subject::Row(row()),
                payload: Value::Null,
            }
        ),
        State::default()
    );
}

#[test]
fn a_split_state_absorbs_back_to_itself_and_each_piece_keeps_to_its_budget() {
    // wsp-702: the pieces of a snapshot too large for one line.
    let mut big = state();
    let base = big.rows[&row()].clone();
    for i in 0..200 {
        let id = RowId(format!("wsp-{i:03}"));
        big.rows.insert(id.clone(), Row { id, ..base.clone() });
    }
    let run = big.runs[0].clone();
    big.runs = (0..20).map(|_| run.clone()).collect();
    let whole = serde_json::to_vec(&big).unwrap().len();
    assert_eq!(big.json_len(), whole, "json_len counts what to_vec writes");
    let one_row = serde_json::to_vec(&base).unwrap().len();
    let budget = one_row * 10;
    let pieces = big.clone().split(budget);
    assert!(pieces.len() > 20, "{} pieces of {whole} B at {budget}", pieces.len());
    for p in &pieces {
        // A piece's JSON is its entities plus the empty collections around them.
        let n = serde_json::to_vec(p).unwrap().len();
        assert!(n <= budget + 200, "a piece of {n} B over a budget of {budget}");
    }
    let mut back = State::default();
    for p in pieces {
        back.absorb(p);
    }
    assert_eq!(back, big, "absorbed in order, the pieces are the state, runs in their order");
    // Small enough, one piece and itself; empty, one empty piece.
    assert_eq!(state().split(usize::MAX), vec![state()]);
    assert_eq!(State::default().split(10), vec![State::default()]);
    // An entity over the budget is a piece alone.
    assert!(big.clone().split(1).iter().all(|p| p.rows.len() + p.runs.len() + p.lists.len() + p.seats.len()
        + p.claims.len() + p.orders.len() + p.asks.len() + p.machines.len() + p.leases.len() == 1));
}
