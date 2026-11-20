//! The orders machine against docs/state-core.md 3.1 (wsp-118): one test per
//! transition of the table, one table of every illegal transition, and the
//! repeat answers.
//!
//! **The stand-in for case 9.** The conformance suite's case 9
//! (`case_machine_guards`) is fixed to its `TestDomain` (`Harness::fresh`
//! builds the store with it), so it cannot run against `Orders`.
//! `illegal_transitions_are_refused_with_their_name_and_write_nothing` stands
//! in for it: the same guarantee, through [`Log`], a model of a backend's
//! `apply` (decide on the state, stamp each draft as the next event, fold it
//! with `evolve`). Each illegal command must be refused with exactly its
//! [`Refusal`] and leave the log's head and the state as they were.

use serde_json::{json, Value};
use std::result::Result;
use wsp_machines::{Orders, ATTEMPTS};
use wsp_store_contract::*;

/// A state's wire name (`queued`), as [`Refusal::Illegal`] carries it.
macro_rules! wire {
    ($state:expr) => {
        serde_json::to_value($state).unwrap().as_str().unwrap().to_string()
    };
}

/// A backend's `apply`, in memory: decide, stamp, fold (`wsp-store-contract`'s `domain` module).
struct Log {
    state: State,
    events: Vec<Event>,
}

const SEAT: &str = "cpd-1";
const OTHER: &str = "cpd-2";

fn ed() -> Principal {
    Principal::Person("ed".into())
}

/// Who holds `deliver:<seat>` in the world: the daemon's delivery worker.
fn worker() -> Principal {
    Principal::Wsp("deliver".into())
}

fn seat_id(s: &str) -> SeatId {
    SeatId(s.into())
}

fn seat(id: &str, state: SeatState) -> Seat {
    Seat {
        id: seat_id(id),
        machine: MachineName("local".into()),
        kind: "claude".into(),
        model: None,
        state,
        before_lost: None,
        row: None,
        attempt: 0,
        pid: None,
        fields: Default::default(),
    }
}

impl Log {
    /// A world with seat `cpd-1` in `state`, seat `cpd-2` idle, and the
    /// worker holding both seats' `deliver:` leases at token 7.
    fn with_seat(state: SeatState) -> Log {
        let mut s = State::default();
        for (id, st) in [(SEAT, state), (OTHER, SeatState::Idle)] {
            s.seats.insert(seat_id(id), seat(id, st));
        }
        let mut log = Log { state: s, events: vec![] };
        for id in [SEAT, OTHER] {
            log.take(&seat_id(id), worker(), Token(7));
        }
        log
    }

    fn new() -> Log {
        Log::with_seat(SeatState::Idle)
    }

    fn head(&self) -> EventId {
        EventId(self.events.len() as u64)
    }

    fn fold(&mut self, by: &Principal, d: Draft) -> EventId {
        let e = Event {
            id: EventId(self.events.len() as u64 + 1),
            at: At(1_000 + self.events.len() as u64),
            principal: by.clone(),
            kind: d.kind,
            subject: d.subject,
            payload: d.payload,
        };
        self.state = Orders.evolve(std::mem::take(&mut self.state), &e);
        self.events.push(e);
        self.head()
    }

    /// What a store writes itself for `take` (the contract's `store` module docs).
    fn take(&mut self, seat: &SeatId, holder: Principal, token: Token) {
        let name = LeaseName::deliver(seat);
        let lease = Lease { name: name.clone(), holder: holder.clone(), token, expires: At(u64::MAX) };
        let d = Draft {
            kind: EventKind::LeaseTaken,
            subject: Subject::Lease(name),
            payload: serde_json::to_value(&lease).unwrap(),
        };
        self.fold(&holder, d);
    }

    fn apply_by(&mut self, cmd: Command, by: &Principal) -> Result<Vec<EventId>, Refusal> {
        let drafts = Orders.decide(&self.state, &cmd, by)?;
        Ok(drafts.into_iter().map(|d| self.fold(by, d)).collect())
    }

    fn apply(&mut self, cmd: Command) -> Result<Vec<EventId>, Refusal> {
        let by = match cmd {
            Command::OrderSend { .. } | Command::OrderCancel { .. } => ed(),
            _ => worker(),
        };
        self.apply_by(cmd, &by)
    }

    /// Applies a command the machine must allow.
    fn ok(&mut self, cmd: Command) -> Vec<EventId> {
        let what = format!("{cmd:?}");
        self.apply(cmd).unwrap_or_else(|r| panic!("{what} was refused {r:?}"))
    }

    fn event(&self, id: EventId) -> &Event {
        &self.events[id.0 as usize - 1]
    }

    fn order(&self, id: &OrderId) -> &Order {
        &self.state.orders[id]
    }

    /// Sends an order to `to` and returns its id.
    fn send_to(&mut self, to: &str, first: bool) -> OrderId {
        let ids = self.ok(Command::OrderSend { to: seat_id(to), text: "go".into(), first });
        match &self.event(ids[0]).subject {
            Subject::Order(id) => id.clone(),
            other => panic!("order.send's event names {other:?}"),
        }
    }

    fn send(&mut self) -> OrderId {
        self.send_to(SEAT, false)
    }

    /// Moves `order` along the happy path until it is in `state`, from `queued`.
    fn bring(&mut self, order: &OrderId, state: OrderState) {
        use OrderState::*;
        let steps: Vec<Command> = match state {
            Queued => vec![],
            Typed => vec![deliver(order)],
            Delivered => vec![deliver(order), delivered(order)],
            Acknowledged => vec![deliver(order), delivered(order), acked(order)],
            Done => vec![deliver(order), delivered(order), acked(order), done(order)],
            Cancelled => vec![Command::OrderCancel { order: order.clone() }],
            Failed => (0..ATTEMPTS).flat_map(|_| [deliver(order), refused(order, "dead", true)]).collect(),
        };
        for cmd in steps {
            self.ok(cmd);
        }
        assert_eq!(self.order(order).state, state);
    }

    /// A new order to `cpd-1`, brought to `state`.
    fn order_in(&mut self, state: OrderState) -> OrderId {
        let o = self.send();
        self.bring(&o, state);
        o
    }
}

fn fence(seat: &str) -> Fence {
    Fence { lease: LeaseName::deliver(&seat_id(seat)), token: Token(7) }
}

fn deliver(order: &OrderId) -> Command {
    let seat = order.0.split("-o").next().unwrap().to_string();
    Command::OrderDeliver { order: order.clone(), fence: fence(&seat) }
}

fn delivered(order: &OrderId) -> Command {
    Command::OrderDelivered { order: order.clone() }
}

fn acked(order: &OrderId) -> Command {
    Command::OrderAcked { order: order.clone(), turn_began: At(5) }
}

fn refused(order: &OrderId, why: &str, counts: bool) -> Command {
    Command::OrderRefused { order: order.clone(), why: why.into(), counts }
}

fn done(order: &OrderId) -> Command {
    Command::OrderDone { order: order.clone() }
}

fn cancel(order: &OrderId) -> Command {
    Command::OrderCancel { order: order.clone() }
}

/// The one event `ids` names.
fn one<'l>(log: &'l Log, ids: &[EventId]) -> &'l Event {
    assert_eq!(ids.len(), 1, "one event, not {ids:?}");
    log.event(ids[0])
}

// ---- The transitions of 3.1, one test each.

#[test]
fn send_queues_an_order_for_a_seat_the_state_holds() {
    let mut log = Log::new();
    let ids = log.ok(Command::OrderSend { to: seat_id(SEAT), text: "go".into(), first: true });
    let e = one(&log, &ids);
    assert_eq!(e.kind, EventKind::OrderQueued);
    let id = OrderId(format!("{SEAT}-o1"));
    assert_eq!(e.subject, Subject::Order(id.clone()));
    let want = Order {
        id: id.clone(),
        from: ed(),
        to: seat_id(SEAT),
        text: "go".into(),
        state: OrderState::Queued,
        attempts: 0,
        first: true,
        why: None,
    };
    assert_eq!(serde_json::from_value::<Order>(e.payload.clone()).unwrap(), want);
    assert_eq!(log.order(&id), &want);
}

#[test]
fn send_queues_for_any_seat_state_but_gone() {
    use SeatState::*;
    for st in [Requested, Spawning, Started, Working, Idle, Ending, Lost] {
        let mut log = Log::with_seat(st);
        let o = log.send();
        assert_eq!(log.order(&o).state, OrderState::Queued, "a seat {st:?}");
    }
}

#[test]
fn order_ids_count_per_seat_and_never_reuse_one_the_state_holds() {
    let mut log = Log::new();
    assert_eq!(log.send().0, "cpd-1-o1");
    assert_eq!(log.send().0, "cpd-1-o2");
    assert_eq!(log.send_to(OTHER, false).0, "cpd-2-o1");
    // A mirrored order under a name the count would reach is stepped past.
    let mut taken = log.order(&OrderId("cpd-1-o1".into())).clone();
    taken.id = OrderId("cpd-1-o4".into());
    log.state.orders.insert(taken.id.clone(), taken);
    assert_eq!(log.send().0, "cpd-1-o5");
}

#[test]
fn deliver_types_a_queued_order_to_an_idle_seat() {
    let mut log = Log::new();
    let o = log.send();
    let ids = log.ok(deliver(&o));
    let e = one(&log, &ids).clone();
    assert_eq!((e.kind, e.subject, e.payload), (EventKind::OrderTyped, Subject::Order(o.clone()), Value::Null));
    assert_eq!(log.order(&o).state, OrderState::Typed);
}

#[test]
fn deliver_types_to_a_working_seat() {
    let mut log = Log::with_seat(SeatState::Working);
    let o = log.send();
    log.ok(deliver(&o));
    assert_eq!(log.order(&o).state, OrderState::Typed);
}

#[test]
fn deliver_types_a_first_order_to_a_started_seat() {
    let mut log = Log::with_seat(SeatState::Started);
    let o = log.send_to(SEAT, true);
    log.ok(deliver(&o));
    assert_eq!(log.order(&o).state, OrderState::Typed);
}

#[test]
fn deliver_types_to_one_seat_while_another_has_an_order_typed() {
    let mut log = Log::new();
    let theirs = log.send_to(OTHER, false);
    log.ok(deliver(&theirs));
    let o = log.send();
    log.ok(deliver(&o));
    assert_eq!(log.order(&o).state, OrderState::Typed);
}

#[test]
fn delivered_moves_a_typed_order_to_delivered() {
    let mut log = Log::new();
    let o = log.order_in(OrderState::Typed);
    let ids = log.ok(delivered(&o));
    assert_eq!(one(&log, &ids).kind, EventKind::OrderDelivered);
    assert_eq!(log.order(&o).state, OrderState::Delivered);
}

#[test]
fn acked_moves_a_typed_order_to_acknowledged() {
    let mut log = Log::new();
    let o = log.order_in(OrderState::Typed);
    let ids = log.ok(acked(&o));
    let e = one(&log, &ids);
    assert_eq!((e.kind, &e.payload), (EventKind::OrderAcknowledged, &json!({ "turn_began": 5 })));
    assert_eq!(log.order(&o).state, OrderState::Acknowledged);
}

#[test]
fn acked_moves_a_delivered_order_to_acknowledged() {
    let mut log = Log::new();
    let o = log.order_in(OrderState::Delivered);
    log.ok(acked(&o));
    assert_eq!(log.order(&o).state, OrderState::Acknowledged);
}

#[test]
fn a_counted_refusal_requeues_with_one_more_attempt_and_its_words() {
    let mut log = Log::new();
    let o = log.order_in(OrderState::Typed);
    let ids = log.ok(refused(&o, "the composer was not empty", true));
    let e = one(&log, &ids);
    assert_eq!(e.kind, EventKind::OrderRefused);
    assert_eq!(e.payload, json!({ "why": "the composer was not empty", "counts": true, "attempts": 1 }));
    let got = log.order(&o);
    assert_eq!((got.state, got.attempts, got.why.as_deref()), (OrderState::Queued, 1, Some("the composer was not empty")));
}

#[test]
fn the_counted_refusal_at_the_cap_fails_the_order() {
    assert_eq!(ATTEMPTS, 3, "part 3 of the person's answer on wsp-115");
    let mut log = Log::new();
    let o = log.send();
    for n in 1..ATTEMPTS {
        log.ok(deliver(&o));
        log.ok(refused(&o, &format!("no {n}"), true));
        assert_eq!((log.order(&o).state, log.order(&o).attempts), (OrderState::Queued, n));
    }
    log.ok(deliver(&o));
    let ids = log.ok(refused(&o, "no 3", true));
    let e = one(&log, &ids);
    assert_eq!((e.kind, &e.payload), (EventKind::OrderFailed, &json!({ "why": "no 3", "attempts": 3 })));
    let got = log.order(&o);
    assert_eq!((got.state, got.attempts, got.why.as_deref()), (OrderState::Failed, ATTEMPTS, Some("no 3")));
}

#[test]
fn an_uncounted_refusal_requeues_with_attempts_unchanged_even_at_the_cap() {
    let mut log = Log::new();
    let o = log.send();
    for n in 1..ATTEMPTS {
        log.ok(deliver(&o));
        log.ok(refused(&o, &format!("no {n}"), true));
    }
    for _ in 0..5 {
        log.ok(deliver(&o));
        let ids = log.ok(refused(&o, "busy", false));
        let e = one(&log, &ids);
        assert_eq!((e.kind, &e.payload), (EventKind::OrderRefused, &json!({ "why": "busy", "counts": false, "attempts": ATTEMPTS - 1 })));
        let got = log.order(&o);
        assert_eq!((got.state, got.attempts, got.why.as_deref()), (OrderState::Queued, ATTEMPTS - 1, Some("busy")));
    }
}

#[test]
fn done_moves_an_acknowledged_order_to_done() {
    let mut log = Log::new();
    let o = log.order_in(OrderState::Acknowledged);
    let ids = log.ok(done(&o));
    assert_eq!(one(&log, &ids).kind, EventKind::OrderDone);
    assert_eq!(log.order(&o).state, OrderState::Done);
}

#[test]
fn cancel_cancels_from_every_state_but_done_and_cancelled() {
    use OrderState::*;
    for st in [Queued, Typed, Delivered, Acknowledged, Failed] {
        let mut log = Log::new();
        let o = log.order_in(st);
        let ids = log.ok(cancel(&o));
        assert_eq!(one(&log, &ids).kind, EventKind::OrderCancelled, "from {st:?}");
        assert_eq!(log.order(&o).state, Cancelled, "from {st:?}");
    }
}

#[test]
fn a_whole_life_folds_to_its_record() {
    let mut log = Log::new();
    let o = log.send();
    log.ok(deliver(&o));
    log.ok(refused(&o, "busy", false));
    log.ok(deliver(&o));
    log.ok(delivered(&o));
    log.ok(acked(&o));
    log.ok(done(&o));
    let got = log.order(&o);
    assert_eq!((got.state, got.attempts, got.why.as_deref()), (OrderState::Done, 0, Some("busy")));
    // Two lease events, then send, deliver, refused, deliver, delivered, acked, done.
    assert_eq!(log.events.len(), 2 + 7);
}

// ---- Every illegal transition: case 9's guarantee, against `Orders`.

#[test]
fn illegal_transitions_are_refused_with_their_name_and_write_nothing() {
    use OrderState::*;
    let illegal = |subject: Subject, state: &str, verb: &str| Refusal::Illegal {
        subject,
        state: state.into(),
        verb: verb.into(),
    };
    let order = |o: &OrderId| Subject::Order(o.clone());
    let seat_s = |s: &str| Subject::Seat(seat_id(s));
    let send = |to: &str, first: bool| Command::OrderSend { to: seat_id(to), text: "go".into(), first };

    /// One illegal case: the world, the command and its principal, the refusal.
    struct Case {
        what: String,
        log: Log,
        cmd: Command,
        by: Principal,
        want: Refusal,
    }
    let mut cases: Vec<Case> = vec![];
    let mut case = |what: String, log: Log, cmd: Command, want: Refusal| {
        let by = match cmd {
            Command::OrderSend { .. } | Command::OrderCancel { .. } => ed(),
            _ => worker(),
        };
        cases.push(Case { what, log, cmd, by, want });
    };

    // order.send: no seat, or a gone one.
    case("send to a seat the state lacks".into(), Log::new(), send("cpd-9", false), Refusal::NotFound { subject: seat_s("cpd-9") });
    case("send to a gone seat".into(), Log::with_seat(SeatState::Gone), send(SEAT, false), illegal(seat_s(SEAT), "gone", "order.send"));

    // Every verb on an order the state lacks.
    let ghost = OrderId("cpd-1-o9".into());
    for cmd in [deliver(&ghost), delivered(&ghost), acked(&ghost), refused(&ghost, "x", true), done(&ghost), cancel(&ghost)] {
        let what = format!("{cmd:?} on an order the state lacks");
        case(what, Log::new(), cmd, Refusal::NotFound { subject: order(&ghost) });
    }

    // order.deliver from any state but queued.
    for st in [Typed, Delivered, Acknowledged, Done, Cancelled, Failed] {
        let mut log = Log::new();
        let o = log.order_in(st);
        let cmd = deliver(&o);
        case(format!("deliver from {st:?}"), log, cmd, illegal(order(&o), &wire!(&st), "order.deliver"));
    }
    // order.deliver to a seat not ready (and `started` for an order not first).
    for st in [SeatState::Requested, SeatState::Spawning, SeatState::Started, SeatState::Ending, SeatState::Gone, SeatState::Lost] {
        let mut log = Log::with_seat(SeatState::Idle);
        let o = log.send();
        log.state.seats.get_mut(&seat_id(SEAT)).unwrap().state = st;
        let cmd = deliver(&o);
        case(format!("deliver to a seat {st:?}"), log, cmd, illegal(seat_s(SEAT), &wire!(&st), "order.deliver"));
    }
    for st in [SeatState::Requested, SeatState::Spawning, SeatState::Ending, SeatState::Lost] {
        let mut log = Log::with_seat(SeatState::Idle);
        let o = log.send_to(SEAT, true);
        log.state.seats.get_mut(&seat_id(SEAT)).unwrap().state = st;
        let cmd = deliver(&o);
        case(format!("deliver a first order to a seat {st:?}"), log, cmd, illegal(seat_s(SEAT), &wire!(&st), "order.deliver"));
    }
    // order.deliver while the seat has another order typed (busy).
    for st in [SeatState::Idle, SeatState::Working] {
        let mut log = Log::with_seat(st);
        log.order_in(Typed);
        let o = log.send();
        let cmd = deliver(&o);
        case(format!("deliver to a {st:?} seat with an order typed"), log, cmd, illegal(seat_s(SEAT), &wire!(&st), "order.deliver"));
    }
    // order.deliver to a seat the state lost after the order was sent.
    {
        let mut log = Log::new();
        let o = log.send();
        log.state.seats.remove(&seat_id(SEAT));
        let cmd = deliver(&o);
        case("deliver to a seat the state no longer holds".into(), log, cmd, Refusal::NotFound { subject: seat_s(SEAT) });
    }
    // order.deliver's fence: another seat's lease, a lease nobody holds, held by another, a stale token.
    {
        let mut log = Log::new();
        let o = log.send();
        let cmd = Command::OrderDeliver { order: o.clone(), fence: fence(OTHER) };
        let reason = format!("{} is delivered under deliver:{SEAT}, and the fence names deliver:{OTHER}", o.0);
        case("deliver under another seat's lease".into(), log, cmd, Refusal::Invalid { reason });

        let mut log = Log::new();
        let o = log.send();
        log.state.leases.remove(&LeaseName::deliver(&seat_id(SEAT)));
        let cmd = deliver(&o);
        case("deliver under a lease nobody holds".into(), log, cmd, Refusal::NotFound { subject: Subject::Lease(LeaseName::deliver(&seat_id(SEAT))) });

        let mut log = Log::new();
        let o = log.send();
        log.take(&seat_id(SEAT), Principal::Wsp("other".into()), Token(7));
        let cmd = deliver(&o);
        let want = Refusal::Held { lease: LeaseName::deliver(&seat_id(SEAT)), holder: Principal::Wsp("other".into()) };
        case("deliver under a lease another holds".into(), log, cmd, want);

        let mut log = Log::new();
        let o = log.send();
        log.take(&seat_id(SEAT), worker(), Token(8));
        let cmd = deliver(&o);
        case("deliver under a stale token".into(), log, cmd, Refusal::Stale { lease: LeaseName::deliver(&seat_id(SEAT)), current: Token(8) });
    }

    // The answers, from the states they do not leave (and are no repeat in).
    for (st, cmds) in [
        (Queued, vec![delivered as fn(&OrderId) -> Command, acked, done]),
        (Typed, vec![done]),
        (Delivered, vec![done]),
        (Cancelled, vec![delivered, acked, done]),
        (Failed, vec![delivered, acked, done]),
    ] {
        for f in cmds {
            let mut log = Log::new();
            let o = log.order_in(st);
            let cmd = f(&o);
            let v = verb(&cmd);
            case(format!("{v} from {st:?}"), log, cmd, illegal(order(&o), &wire!(&st), &v));
        }
    }
    // order.refused from any state but typed, and on queued or failed with other words than its last refusal's.
    for st in [Queued, Delivered, Acknowledged, Done, Cancelled, Failed] {
        let mut log = Log::new();
        let o = log.order_in(st);
        let cmd = refused(&o, "other words", true);
        case(format!("refused from {st:?}"), log, cmd, illegal(order(&o), &wire!(&st), "order.refused"));
    }
    // order.cancel from done or cancelled.
    for st in [Done, Cancelled] {
        let mut log = Log::new();
        let o = log.order_in(st);
        let cmd = cancel(&o);
        case(format!("cancel from {st:?}"), log, cmd, illegal(order(&o), &wire!(&st), "order.cancel"));
    }
    // Not the machine's: mirror (owned), and another machine's verb.
    {
        let put = vec![Entity::Seat(seat(SEAT, SeatState::Idle))];
        case("mirror".into(), Log::new(), Command::Mirror { put, gone: vec![] }, illegal(seat_s(SEAT), "owned", "mirror"));
        let cmd = Command::RowPark { row: RowId("wsp-1".into()) };
        let want = Refusal::Invalid { reason: "row.park is not the orders machine's".into() };
        case("row.park".into(), Log::new(), cmd, want);
    }

    assert!(cases.len() >= 50, "the table has {} cases", cases.len());
    for Case { what, mut log, cmd, by, want } in cases {
        let (before, head) = (log.state.clone(), log.head());
        let got = log.apply_by(cmd, &by);
        assert_eq!(got, Err(want), "{what}");
        assert_eq!((log.head(), &log.state), (head, &before), "{what}: refused, yet something was written");
    }
}

// ---- A repeat answer decides nothing.

#[test]
fn an_answer_applied_twice_decides_nothing_the_second_time() {
    use OrderState::*;
    let check = |what: &str, mut log: Log, cmd: Command| {
        let first = log.apply(cmd.clone()).unwrap_or_else(|r| panic!("{what}: the first was refused {r:?}"));
        assert!(!first.is_empty(), "{what}: the first decided nothing");
        let (state, head) = (log.state.clone(), log.head());
        assert_eq!(log.apply(cmd), Ok(vec![]), "{what}: the second");
        assert_eq!((log.head(), &log.state), (head, &state), "{what}: the second wrote something");
    };
    for (what, st, f) in [
        ("delivered", Typed, delivered as fn(&OrderId) -> Command),
        ("acked from typed", Typed, acked),
        ("acked from delivered", Delivered, acked),
        ("done", Acknowledged, done),
    ] {
        let mut log = Log::new();
        let o = log.order_in(st);
        let cmd = f(&o);
        check(what, log, cmd);
    }
    for (what, counts) in [("a counted refusal", true), ("an uncounted refusal", false)] {
        let mut log = Log::new();
        let o = log.order_in(Typed);
        check(what, log, refused(&o, "busy", counts));
    }
    // The refusal that failed the order.
    let mut log = Log::new();
    let o = log.send();
    for n in 1..ATTEMPTS {
        log.ok(deliver(&o));
        log.ok(refused(&o, &format!("no {n}"), true));
    }
    log.ok(deliver(&o));
    check("the refusal at the cap", log, refused(&o, "dead", true));
}

#[test]
fn an_answer_the_order_has_passed_decides_nothing() {
    use OrderState::*;
    for (st, f) in [
        (Acknowledged, delivered as fn(&OrderId) -> Command),
        (Done, delivered),
        (Done, acked),
    ] {
        let mut log = Log::new();
        let o = log.order_in(st);
        let cmd = f(&o);
        let (state, head) = (log.state.clone(), log.head());
        assert_eq!(log.apply(cmd.clone()), Ok(vec![]), "{} on {st:?}", verb(&cmd));
        assert_eq!((log.head(), &log.state), (head, &state));
    }
}

// ---- What the machine owns, and its fold.

#[test]
fn owns_the_order_verbs_and_nothing_else() {
    let o = OrderId("o".into());
    let mine = [
        Command::OrderSend { to: seat_id(SEAT), text: "go".into(), first: false },
        cancel(&o),
        deliver(&OrderId("cpd-1-o1".into())),
        delivered(&o),
        acked(&o),
        refused(&o, "x", true),
        done(&o),
    ];
    for cmd in &mine {
        assert!(Orders::owns(cmd), "{cmd:?}");
    }
    let theirs = [
        Command::Mirror { put: vec![], gone: vec![] },
        Command::RowPark { row: RowId("wsp-1".into()) },
        Command::SeatEnd { seat: seat_id(SEAT), fence: None },
        Command::AskAck { ask: AskId("a-1".into()) },
    ];
    for cmd in &theirs {
        assert!(!Orders::owns(cmd), "{cmd:?}");
    }
}

#[test]
fn evolve_folds_the_lease_events_and_leaves_the_rest() {
    let mut log = Log::new();
    let name = LeaseName::deliver(&seat_id(SEAT));
    assert_eq!(log.state.leases[&name].token, Token(7));
    log.fold(&worker(), Draft { kind: EventKind::LeaseReleased, subject: Subject::Lease(name.clone()), payload: Value::Null });
    assert!(!log.state.leases.contains_key(&name));
    let before = log.state.clone();
    for (kind, subject) in [
        (EventKind::RowParked, Subject::Row(RowId("wsp-1".into()))),
        (EventKind::SeatWorking, Subject::Seat(seat_id(SEAT))),
        (EventKind::OrderTyped, Subject::Order(OrderId("cpd-1-o9".into()))),
        (EventKind::TextSet, Subject::Row(RowId("wsp-1".into()))),
        (EventKind::Unknown, Subject::Seat(seat_id(SEAT))),
    ] {
        log.fold(&worker(), Draft { kind, subject, payload: Value::Null });
    }
    assert_eq!(log.state, before);
}

#[test]
fn the_machine_is_held_as_a_dyn_domain() {
    let d: &dyn Domain = &Orders;
    let mut s = State::default();
    s.seats.insert(seat_id(SEAT), seat(SEAT, SeatState::Idle));
    let drafts = d.decide(&s, &Command::OrderSend { to: seat_id(SEAT), text: "go".into(), first: false }, &ed()).unwrap();
    assert_eq!(drafts.len(), 1);
}

/// A command's verb (`order.deliver`).
fn verb(cmd: &Command) -> String {
    serde_json::to_value(cmd).unwrap()["verb"].as_str().unwrap().to_string()
}
