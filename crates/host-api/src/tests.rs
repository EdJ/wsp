use super::*;
use serde_json::{json, Value};
use std::collections::BTreeSet;
use std::time::Duration;

fn row() -> RowId {
    RowId("wsp-002".into())
}
fn seat() -> SeatId {
    SeatId("cpd-115".into())
}
fn host() -> HostId {
    HostId("mac".into())
}
fn key() -> Key {
    Key {
        row: row(),
        attempt: 2,
    }
}
fn job_key() -> JobKey {
    JobKey {
        row: row(),
        attempt: 2,
        job: "check".into(),
    }
}
fn tree() -> Tree {
    Tree {
        row: row(),
        host: host(),
    }
}
fn order() -> OrderId {
    OrderId("ord-7".into())
}
fn frame(msg: Message) -> Frame {
    Frame {
        id: None,
        req: None,
        msg,
    }
}
fn event(id: u64, msg: Message) -> Frame {
    Frame {
        id: Some(EventId(id)),
        req: None,
        msg,
    }
}
fn request(req: u64, msg: Message) -> Frame {
    Frame {
        id: None,
        req: Some(ReqId(req)),
        msg,
    }
}

/// How many variants `Message` has. [`ordinal`] is an exhaustive match, so a
/// new variant does not compile until it has an arm; give it the next ordinal
/// and bump this, and [`every_message_round_trips`] fails until the table has
/// a frame of it.
const VARIANTS: usize = 31;

fn ordinal(m: &Message) -> (usize, &'static str) {
    match m {
        Message::Hello(_) => (0, "hello"),
        Message::SeatSpawn(_) => (1, "seat.spawn"),
        Message::SeatStarted(_) => (2, "seat.started"),
        Message::SeatRefused(_) => (3, "seat.refused"),
        Message::SeatStop(_) => (4, "seat.stop"),
        Message::SeatStopped(_) => (5, "seat.stopped"),
        Message::OrderDeliver(_) => (6, "order.deliver"),
        Message::OrderCancel(_) => (7, "order.cancel"),
        Message::OrderDelivered(_) => (8, "order.delivered"),
        Message::OrderRefused(_) => (9, "order.refused"),
        Message::OrderAcked(_) => (10, "order.acked"),
        Message::Turn(_) => (11, "turn"),
        Message::SeatSaid(_) => (12, "seat.said"),
        Message::SeatSpent(_) => (13, "seat.spent"),
        Message::SeatDid(_) => (14, "seat.did"),
        Message::SeatStuck(_) => (15, "seat.stuck"),
        Message::Seats(_) => (16, "seats"),
        Message::SeatsList(_) => (17, "seats.list"),
        Message::SeatRead(_) => (18, "seat.read"),
        Message::SeatScreen(_) => (19, "seat.screen"),
        Message::JobRun(_) => (20, "job.run"),
        Message::JobStop(_) => (21, "job.stop"),
        Message::JobStarted(_) => (22, "job.started"),
        Message::JobEnded(_) => (23, "job.ended"),
        Message::TreeCheckout(_) => (24, "tree.checkout"),
        Message::TreeLand(_) => (25, "tree.land"),
        Message::TreeCopy(_) => (26, "tree.copy"),
        Message::Beat(_) => (27, "beat"),
        Message::Subscribe(_) => (28, "subscribe"),
        Message::Refused(_) => (29, "refused"),
    }
}

/// `hello` has two sides, so the manager's is its own row of the coverage
/// count and the table must hold both.
fn coverage(f: &Frame) -> usize {
    match &f.msg {
        Message::Hello(Hello::Manager(_)) => 30,
        m => ordinal(m).0,
    }
}

fn table() -> Vec<Frame> {
    vec![
        frame(Message::Hello(Hello::Wsp(WspHello {
            role: Role::Daemon,
            protocol: PROTOCOL,
            version: "0.1.0".into(),
            last_event: Some(EventId(41)),
            token: Token("s3cret".into()),
        }))),
        frame(Message::Hello(Hello::Manager(ManagerHello {
            host: host(),
            protocol: PROTOCOL,
            version: "0.1.0".into(),
            compound_sup: WireRange { min: 3, max: 4 },
            compound_render: WireRange { min: 1, max: 1 },
            journal_head: Some(EventId(57)),
            seats: vec![SeatEpoch {
                seat: seat(),
                epoch: Epoch(1),
            }],
            capacity: Capacity {
                seats: 12,
                build: 2,
                check: 4,
            },
        }))),
        request(
            1,
            Message::SeatSpawn(SeatSpawn {
                key: key(),
                seat: seat(),
                agent: AgentSpec {
                    argv: vec!["claude".into(), "--model".into(), "opus".into()],
                    env: vec!["CLAUDE_CODE_OAUTH_TOKEN".into()],
                    kind: "claude".into(),
                    model: Some("opus".into()),
                },
                workdir: Workdir::Tree(tree()),
                gate: Gate::Required,
                first_order: Some(FirstOrder {
                    order: order(),
                    text: "begin\nwith two lines".into(),
                }),
            }),
        ),
        request(
            2,
            Message::SeatSpawn(SeatSpawn {
                key: Key {
                    row: RowId("wsp-003".into()),
                    attempt: 0,
                },
                seat: SeatId("cpd-116".into()),
                agent: AgentSpec {
                    argv: vec!["opencode".into()],
                    env: vec![],
                    kind: "opencode".into(),
                    model: None,
                },
                workdir: Workdir::Cwd("/Users/x/src".into()),
                gate: Gate::None,
                first_order: None,
            }),
        ),
        event(
            58,
            Message::SeatStarted(SeatStarted {
                key: key(),
                seat: seat(),
                epoch: Epoch(2),
                pid: 4242,
                sockets: vec!["/exec/cpd-115.sock".into()],
            }),
        ),
        event(
            59,
            Message::SeatRefused(SeatRefused {
                key: key(),
                seat: seat(),
                why: Refusal::Full,
                detail: "12 of 12 seats".into(),
            }),
        ),
        request(
            3,
            Message::SeatStop(SeatStop {
                seat: seat(),
                epoch: Epoch(2),
                grace_secs: 10,
            }),
        ),
        event(
            60,
            Message::SeatStopped(SeatStopped {
                seat: seat(),
                epoch: Epoch(2),
                exit: Some(0),
                ended: vec![4242, 4250],
                renderer_left: true,
            }),
        ),
        request(
            4,
            Message::OrderDeliver(OrderDeliver {
                order: order(),
                seat: seat(),
                epoch: Epoch(2),
                text: "say \"hi\"".into(),
                now: true,
            }),
        ),
        request(
            5,
            Message::OrderCancel(OrderCancel {
                order: order(),
                seat: seat(),
            }),
        ),
        event(
            61,
            Message::OrderDelivered(OrderDelivered {
                order: order(),
                seat: seat(),
            }),
        ),
        event(
            62,
            Message::OrderRefused(OrderRefused {
                order: order(),
                seat: seat(),
                why: OrderFail::NoComposer,
            }),
        ),
        event(
            63,
            Message::OrderAcked(OrderAcked {
                order: order(),
                seat: seat(),
                turn_began: At(1_763_400_000_000),
            }),
        ),
        event(
            64,
            Message::Turn(Turn {
                seat: seat(),
                epoch: Epoch(2),
                edge: TurnEdge::Began,
                at: At(1_763_400_000_000),
                order: Some(order()),
            }),
        ),
        event(
            65,
            Message::Turn(Turn {
                seat: seat(),
                epoch: Epoch(2),
                edge: TurnEdge::Ended,
                at: At(1),
                order: None,
            }),
        ),
        event(
            66,
            Message::SeatSaid(SeatSaid {
                seat: seat(),
                epoch: Epoch(2),
                at: At(2),
                text: "done".into(),
            }),
        ),
        event(
            67,
            Message::SeatSpent(SeatSpent {
                seat: seat(),
                epoch: Epoch(2),
                tokens: Tokens {
                    input: 1200,
                    output: 340,
                    cache_read: 90_000,
                    cache_write: 4_000,
                },
                cost_micros: 1_250_000,
            }),
        ),
        event(
            68,
            Message::SeatDid(SeatDid {
                seat: seat(),
                epoch: Epoch(2),
                at: At(3),
                digest: "edited 2 files".into(),
                transcript: FileRef {
                    host: host(),
                    path: "/u/.claude/projects/x/abc.jsonl".into(),
                },
            }),
        ),
        event(
            69,
            Message::SeatStuck(SeatStuck {
                seat: seat(),
                epoch: Epoch(2),
                command: "wsp wait wsp-002".into(),
                waited_secs: 600,
            }),
        ),
        request(6, Message::Seats(Seats {})),
        request(
            6,
            Message::SeatsList(SeatsList {
                seats: vec![SeatRow {
                    seat: seat(),
                    epoch: Epoch(2),
                    state: SeatState::Working,
                    rss_bytes: 512 << 20,
                    shells: 1,
                    last_turn: Some(At(4)),
                    last_word: Some("building".into()),
                }],
                host: None,
            }),
        ),
        request(7, Message::SeatRead(SeatRead { seat: seat() })),
        request(
            7,
            Message::SeatScreen(SeatScreen {
                seat: seat(),
                epoch: Epoch(2),
                text: "> \u{1b}[1m❯\u{1b}[0m".into(),
            }),
        ),
        request(
            8,
            Message::JobRun(JobRun {
                key: job_key(),
                class: JobClass::Check,
                argv: vec!["cargo".into(), "test".into()],
                tree: tree(),
                timeout_secs: 1800,
            }),
        ),
        request(9, Message::JobStop(JobStop { key: job_key() })),
        event(
            70,
            Message::JobStarted(JobStarted {
                key: job_key(),
                pgid: 5150,
            }),
        ),
        event(
            71,
            Message::JobEnded(JobEnded {
                key: job_key(),
                exit: None,
                timed_out: true,
                log: FileRef {
                    host: host(),
                    path: "/exec/jobs/wsp-002.log".into(),
                },
                cpu_ms: 912_000,
            }),
        ),
        request(
            10,
            Message::TreeCheckout(TreeVerb {
                key: key(),
                tree: tree(),
            }),
        ),
        request(
            11,
            Message::TreeLand(TreeVerb {
                key: key(),
                tree: tree(),
            }),
        ),
        request(
            12,
            Message::TreeCopy(TreeVerb {
                key: key(),
                tree: tree(),
            }),
        ),
        frame(Message::Beat(Beat {
            at: At(5),
            host: None,
            seats: vec![],
        })),
        frame(Message::Beat(Beat {
            at: At(6),
            host: Some(HostFacts {
                load_centi: 431,
                cpus: 10,
                memory: MemoryPressure::Warn,
                swap_used_bytes: 1 << 30,
                disks: vec![DiskFree {
                    volume: "/".into(),
                    free_bytes: 50 << 30,
                    total_bytes: 1 << 40,
                }],
                thermal: Thermal::Fair,
            }),
            seats: vec![SeatRss {
                seat: seat(),
                rss_bytes: 400 << 20,
            }],
        })),
        frame(Message::Subscribe(Subscribe {
            from: Some(EventId(41)),
        })),
        frame(Message::Subscribe(Subscribe { from: None })),
        request(
            10,
            Message::Refused(Refused {
                why: Refusal::Unsupported,
                detail: "tree.checkout before Q6".into(),
            }),
        ),
    ]
}

#[test]
fn every_message_round_trips() {
    let mut seen = BTreeSet::new();
    for f in table() {
        let line = serde_json::to_string(&f).unwrap();
        assert!(!line.contains('\n'), "a frame is one line: {line}");

        let v: Value = serde_json::from_str(&line).unwrap();
        assert_eq!(v["type"], ordinal(&f.msg).1, "wire name of {line}");

        let back: Frame = serde_json::from_str(&line).unwrap();
        assert_eq!(back, f, "round trip of {line}");
        assert_eq!(serde_json::to_string(&back).unwrap(), line);

        let n = coverage(&f);
        assert!(n < VARIANTS, "ordinal {n} past VARIANTS");
        seen.insert(n);
    }
    let missing: Vec<usize> = (0..VARIANTS).filter(|n| !seen.contains(n)).collect();
    assert!(
        missing.is_empty(),
        "no frame in the table for ordinals {missing:?}"
    );
}

/// The wire spelling, pinned: a rename in the types is a protocol change and
/// must show here.
#[test]
fn wire_spelling_is_pinned() {
    let spawn = &table()[2];
    assert_eq!(
        serde_json::to_value(spawn).unwrap(),
        json!({
            "req": 1,
            "type": "seat.spawn",
            "key": {"row": "wsp-002", "attempt": 2},
            "seat": "cpd-115",
            "agent": {
                "argv": ["claude", "--model", "opus"],
                "env": ["CLAUDE_CODE_OAUTH_TOKEN"],
                "kind": "claude",
                "model": "opus"
            },
            "workdir": {"tree": {"row": "wsp-002", "host": "mac"}},
            "gate": "required",
            "first_order": {"order": "ord-7", "text": "begin\nwith two lines"}
        })
    );

    let hello = &table()[1];
    let v = serde_json::to_value(hello).unwrap();
    assert_eq!(v["type"], "hello");
    assert_eq!(v["side"], "manager");
    assert_eq!(v["compound_sup"], json!({"min": 3, "max": 4}));

    let started = serde_json::to_value(&table()[4]).unwrap();
    assert_eq!(started["id"], 58);
    assert!(
        started.get("req").is_none(),
        "an absent envelope field is left out"
    );

    let refused = serde_json::to_value(&table()[11]).unwrap();
    assert_eq!(
        refused["why"], "no_composer",
        "compound-sup's FailWhy spelling"
    );
}

/// An older peer leaves out what was added after it; a newer one sends fields
/// and reasons this build does not know. Both still decode.
#[test]
fn peers_of_other_versions_decode() {
    let older: Frame =
        serde_json::from_str(r#"{"type":"seat.stopped","seat":"cpd-1","epoch":3}"#).unwrap();
    assert_eq!(
        older.msg,
        Message::SeatStopped(SeatStopped {
            seat: SeatId("cpd-1".into()),
            epoch: Epoch(3),
            exit: None,
            ended: vec![],
            renderer_left: false
        })
    );

    let newer: Frame = serde_json::from_str(
        r#"{"id":9,"type":"order.refused","order":"o","seat":"cpd-1","why":"window_on_fire","extra":true}"#,
    )
    .unwrap();
    assert_eq!(newer.id, Some(EventId(9)));
    match newer.msg {
        Message::OrderRefused(r) => assert_eq!(r.why, OrderFail::Unknown),
        m => panic!("decoded as {m:?}"),
    }

    let refused: Frame = serde_json::from_str(r#"{"type":"refused","why":"too_tired"}"#).unwrap();
    assert_eq!(
        refused.msg,
        Message::Refused(Refused {
            why: Refusal::Unknown,
            detail: String::new()
        })
    );

    assert!(serde_json::from_str::<Frame>(r#"{"type":"seat.teleport","seat":"cpd-1"}"#).is_err());
}

#[test]
fn token_is_not_printed() {
    let f = &table()[0];
    let shown = format!("{f:?}");
    assert!(!shown.contains("s3cret"), "{shown}");
    assert!(shown.contains("Token(..)"));
}

#[test]
fn memory_pair_carries_every_frame() {
    let (mut a, mut b) = pair();
    for f in table() {
        a.send(&f).unwrap();
        assert_eq!(b.recv(Duration::from_secs(1)).unwrap(), Some(f.clone()));
        b.send(&f).unwrap();
        assert_eq!(a.recv(Duration::from_secs(1)).unwrap(), Some(f));
    }
}

#[test]
fn memory_pair_quiet_malformed_and_closed() {
    let (mut a, mut b) = pair();
    assert_eq!(
        b.recv(Duration::from_millis(10)).unwrap(),
        None,
        "a quiet wait is not an error"
    );

    a.send_line("{\"type\":\"beat\"\n").unwrap();
    assert!(matches!(
        b.recv(Duration::from_secs(1)),
        Err(ConnError::Malformed(_))
    ));
    a.send_line("{\"type\":\"seats\"}").unwrap();
    assert!(
        matches!(b.recv(Duration::from_secs(1)), Err(ConnError::Malformed(_))),
        "no newline"
    );
    a.send_line("{\"type\":\"seats\"}\n{\"type\":\"seats\"}\n")
        .unwrap();
    assert!(
        matches!(b.recv(Duration::from_secs(1)), Err(ConnError::Malformed(_))),
        "two frames on one line"
    );

    let good = frame(Message::Seats(Seats {}));
    a.send(&good).unwrap();
    assert_eq!(
        b.recv(Duration::from_secs(1)).unwrap(),
        Some(good),
        "still usable after a bad line"
    );

    drop(a);
    assert_eq!(b.recv(Duration::from_secs(1)), Err(ConnError::Closed));
    assert_eq!(
        b.send(&frame(Message::Seats(Seats {}))),
        Err(ConnError::Closed)
    );
}
