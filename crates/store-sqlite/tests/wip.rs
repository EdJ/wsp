//! `wip_snapshot` (wsp-695): what `wip` draws of the state, cut in SQL. Open rows, lists
//! not finished and finished lists marked `WIP_SEATED` keep their bodies, every other row
//! and list loses its `WIP_BODIES` fields and
//! keeps the rest, only
//! raised asks come back, seats and claims are whole, and the rest is empty. `snapshot()` is
//! unchanged beside it, and the in-memory store cuts the same state to the same reply.
//!
//! And wsp-743's reads over the same seed: `rows` and `lists` as heads (each less its text,
//! cut in SQL), and readings, kept beside the state with no event.

use std::collections::BTreeMap;
use std::path::PathBuf;
use std::sync::Arc;

use serde_json::Value;
use wsp_store_conformance::MemStore;
use wsp_store_contract::*;
use wsp_store_sqlite::SqliteStore;

/// `mirror` alone: each put is a `mirror.put` carrying the entity, folded in place.
struct Mirror;

fn subject(e: &Entity) -> Subject {
    match e {
        Entity::Row(r) => Subject::Row(r.id.clone()),
        Entity::List(l) => Subject::List(l.id.clone()),
        Entity::Seat(s) => Subject::Seat(s.id.clone()),
        Entity::Claim(c) => Subject::Claim(c.row.clone()),
        Entity::Order(o) => Subject::Order(o.id.clone()),
        Entity::Ask(a) => Subject::Ask(a.id.clone()),
        Entity::Machine(m) => Subject::Machine(m.name.clone()),
    }
}

impl Domain for Mirror {
    fn decide(&self, _: &State, cmd: &Command, _: &Principal) -> std::result::Result<Vec<Draft>, Refusal> {
        let Command::Mirror { put, .. } = cmd else { return Ok(vec![]) };
        Ok(put
            .iter()
            .map(|e| Draft { kind: EventKind::MirrorPut, subject: subject(e), payload: serde_json::to_value(e).unwrap() })
            .collect())
    }

    fn evolve(&self, mut s: State, ev: &Event) -> State {
        if ev.kind != EventKind::MirrorPut {
            return s;
        }
        match serde_json::from_value::<Entity>(ev.payload.clone()).unwrap() {
            Entity::Row(r) => drop(s.rows.insert(r.id.clone(), r)),
            Entity::List(l) => drop(s.lists.insert(l.id.clone(), l)),
            Entity::Seat(x) => drop(s.seats.insert(x.id.clone(), x)),
            Entity::Claim(c) => drop(s.claims.insert(c.row.clone(), c)),
            Entity::Order(o) => drop(s.orders.insert(o.id.clone(), o)),
            Entity::Ask(a) => drop(s.asks.insert(a.id.clone(), a)),
            Entity::Machine(m) => drop(s.machines.insert(m.name.clone(), m)),
        }
        s
    }
}

fn fields(kv: &[(&str, &str)]) -> BTreeMap<String, String> {
    kv.iter().map(|(k, v)| (k.to_string(), v.to_string())).collect()
}

fn row(id: &str, state: RowState, kv: &[(&str, &str)]) -> Entity {
    Entity::Row(Row {
        id: RowId(id.into()),
        project: ProjectId("wsp".into()),
        title: format!("{id}'s title"),
        state,
        before_block: None,
        seat: None,
        group: None,
        commit: Some("abc123".into()),
        fields: fields(kv),
    })
}

fn ask(id: &str, state: AskState) -> Entity {
    Entity::Ask(Ask {
        id: AskId(id.into()),
        kind: AskKind::Ask,
        from: Principal::Seat(SeatId("cpd-1".into())),
        to: None,
        about: Some(Subject::Row(RowId("open".into()))),
        text: format!("{id}?"),
        state,
        answer: None,
        fields: fields(&[("record", "{}")]),
    })
}

/// One of everything the cut treats apart: a row in each state, a done row with a body and one
/// with a head and prose, a running list and a finished one with bodies, an ask in each state, a seat, a claim, an order and
/// a machine.
fn world() -> Vec<Entity> {
    let mut es: Vec<Entity> = [
        RowState::Todo,
        RowState::Doing,
        RowState::Review,
        RowState::SentBack,
        RowState::Blocked,
        RowState::Parked,
    ]
    .into_iter()
    .map(|st| row(&format!("open-{st:?}"), st, &[("body", "---\nid: x\n---\nThe body."), ("tags", "a,b")]))
    .collect();
    es.push(row("open", RowState::Doing, &[("body", "open body"), ("prose", "and prose"), ("claim", "{\"agent_id\":\"cpd-1\"}")]));
    es.push(row("done-body", RowState::Done, &[("body", "a long finished body"), ("tags", "verify"), ("caption", "landed")]));
    es.push(row("done-head", RowState::Done, &[("head", "{\"id\":\"done-head\"}"), ("prose", "the prose"), ("updated", "2026-11-20")]));
    es.push(Entity::List(List {
        id: ListId("wsp-read-api".into()),
        state: ListState::Running,
        groups: vec![Group {
            id: GroupId { list: ListId("wsp-read-api".into()), n: 2 },
            state: ListState::Running,
            members: vec![RowId("open".into())],
            attempt: 1,
            fields: fields(&[("caption", "group two"), ("stop", "barrier")]),
        }],
        fields: fields(&[("body", "# the list's file"), ("title", "read api"), ("status", "running")]),
    }));
    es.push(Entity::List(List {
        id: ListId("old-list".into()),
        state: ListState::Finished,
        groups: vec![Group {
            id: GroupId { list: ListId("old-list".into()), n: 1 },
            state: ListState::Passed,
            members: vec![RowId("done-body".into())],
            attempt: 1,
            fields: BTreeMap::new(),
        }],
        fields: fields(&[("body", "# a finished list's file"), ("title", "old"), ("status", "done")]),
    }));
    es.push(Entity::List(List {
        id: ListId("seated-list".into()),
        state: ListState::Finished,
        groups: vec![],
        fields: fields(&[("body", "# a finished list a check still sits on"), ("status", "done"), (WIP_SEATED, "true")]),
    }));
    for (id, st) in [("q-raised", AskState::Raised), ("q-acked", AskState::Acked), ("q-answered", AskState::Answered)] {
        es.push(ask(id, st));
    }
    es.push(Entity::Seat(Seat {
        id: SeatId("cpd-1".into()),
        machine: MachineName("local".into()),
        kind: "claude".into(),
        model: Some("opus".into()),
        state: SeatState::Working,
        before_lost: None,
        row: Some(RowId("open".into())),
        attempt: 0,
        pid: Some(42),
        fields: fields(&[("role", "worker")]),
    }));
    es.push(Entity::Claim(Claim { row: RowId("open".into()), seat: SeatId("cpd-1".into()), token: Token(3), expires: At(9_000) }));
    es.push(Entity::Order(Order {
        id: OrderId("o-1".into()),
        from: Principal::Wsp("wsp".into()),
        to: SeatId("cpd-1".into()),
        text: "go".into(),
        state: OrderState::Queued,
        attempts: 0,
        first: false,
        why: None,
    }));
    es.push(Entity::Machine(Machine { name: MachineName("local".into()), transport: Transport::Local, beat: Some(At(1)), facts: BTreeMap::new() }));
    es
}

fn seeded<S: Store>(s: &S) {
    let cmd = Command::Mirror { put: world(), gone: vec![] };
    s.apply(cmd, IdempotencyKey("seed".into()), Principal::Wsp("test".into())).unwrap();
}

fn sqlite(tag: &str) -> SqliteStore {
    let dir = PathBuf::from(env!("CARGO_TARGET_TMPDIR"));
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join(format!("wip-{tag}-{}.db", std::process::id()));
    for ext in ["", "-wal", "-shm"] {
        let _ = std::fs::remove_file(format!("{}{ext}", path.display()));
    }
    SqliteStore::open(path, Arc::new(Mirror)).unwrap()
}

#[test]
fn wip_snapshot_keeps_bodies_on_open_rows_alone_and_raised_asks_alone() {
    let s = sqlite("shape");
    seeded(&s);
    let (whole, at) = s.snapshot().unwrap();
    let (wip, wip_at) = s.wip_snapshot().unwrap();
    assert_eq!(wip_at, at, "the cut is of the snapshot's moment");

    // Every row is there; bodies only on the open ones.
    assert_eq!(wip.rows.keys().collect::<Vec<_>>(), whole.rows.keys().collect::<Vec<_>>());
    for (key, r) in &wip.rows {
        let id = &key.0;
        let w = &whole.rows[key];
        if r.state == RowState::Done {
            for k in WIP_BODIES {
                assert!(!r.fields.contains_key(k), "{id}: a done row keeps no `{k}`");
            }
            let mut head = w.fields.clone();
            WIP_BODIES.iter().for_each(|k| drop(head.remove(*k)));
            assert_eq!(r, &Row { fields: head, ..w.clone() }, "{id}: the head is the row less its text");
        } else {
            assert_eq!(r, w, "{id}: an open row is whole");
            assert!(r.fields.contains_key("body"), "{id}: an open row keeps its body");
        }
    }
    assert_eq!(wip.rows[&RowId("open".into())].fields["prose"], "and prose");
    assert_eq!(wip.rows[&RowId("done-head".into())].fields["head"], "{\"id\":\"done-head\"}");
    assert_eq!(wip.rows[&RowId("done-body".into())].fields["tags"], "verify");

    // A list not finished is whole: its body is the only way back to its worklist.
    assert_eq!(wip.lists.keys().collect::<Vec<_>>(), whole.lists.keys().collect::<Vec<_>>());
    let running = ListId("wsp-read-api".into());
    assert_eq!(wip.lists[&running], whole.lists[&running], "a running list is whole");
    assert_eq!(wip.lists[&running].fields["body"], "# the list's file");
    // A finished list without its text, groups whole.
    let list = &wip.lists[&ListId("old-list".into())];
    let whole_list = &whole.lists[&ListId("old-list".into())];
    for k in WIP_BODIES {
        assert!(!list.fields.contains_key(k), "a finished list keeps no `{k}`");
    }
    assert_eq!(list.fields, fields(&[("title", "old"), ("status", "done")]));
    assert_eq!((&list.groups, list.state), (&whole_list.groups, whole_list.state));
    // A finished list a seat still sits on (`WIP_SEATED`) is whole, as wip reads it.
    let seated = ListId("seated-list".into());
    assert_eq!(wip.lists[&seated], whole.lists[&seated], "a finished list with a seat on it is whole");

    // Only the raised ask.
    assert_eq!(wip.asks.keys().map(|a| a.0.as_str()).collect::<Vec<_>>(), ["q-raised"]);
    assert_eq!(wip.asks.values().next(), whole.asks.get(&AskId("q-raised".into())));

    // Seats and claims whole; the rest empty.
    assert_eq!((&wip.seats, &wip.claims), (&whole.seats, &whole.claims));
    assert!(wip.orders.is_empty() && wip.runs.is_empty() && wip.machines.is_empty() && wip.leases.is_empty());
    assert!(!whole.orders.is_empty() && !whole.machines.is_empty(), "the seed has what the cut leaves out");

    // `snapshot()` is not touched: every body and every ask is still in it.
    assert_eq!(whole.rows[&RowId("done-body".into())].fields["body"], "a long finished body");
    assert_eq!(whole.lists[&ListId("wsp-read-api".into())].fields["body"], "# the list's file");
    assert_eq!(whole.asks.len(), 3);
}

#[test]
fn the_sql_cut_and_the_memory_cut_agree() {
    let s = sqlite("agree");
    let m = MemStore::with_domain(Arc::new(Mirror));
    seeded(&s);
    seeded(&m);
    assert_eq!(s.snapshot().unwrap().0, m.snapshot().unwrap().0, "the two stores hold one state");
    assert_eq!(s.wip_snapshot().unwrap().0, m.wip_snapshot().unwrap().0);
    // The cut is a `State` on the wire as `snapshot()`'s is: it decodes as one.
    let line = serde_json::to_string(&s.wip_snapshot().unwrap().0).unwrap();
    let back: Value = serde_json::from_str(&line).unwrap();
    assert!(back["rows"]["done-body"]["fields"].get("body").is_none());
    assert!(back["lists"]["old-list"]["fields"].get("body").is_none());
    assert!(back["lists"]["wsp-read-api"]["fields"].get("body").is_some());
    assert!(back["lists"]["seated-list"]["fields"].get("body").is_some());
}

#[test]
fn an_empty_store_cuts_to_the_empty_state() {
    let s = sqlite("empty");
    assert_eq!(s.wip_snapshot().unwrap(), (State::default(), EventId(0)));
}

// ---------------------------------------------------------------- wsp-743: heads and readings

/// The fields `heads` keeps: everything but the text.
fn head_of(mut f: BTreeMap<String, String>) -> BTreeMap<String, String> {
    WIP_BODIES.iter().for_each(|k| drop(f.remove(*k)));
    f
}

/// `rows {heads: true}` is every row `rows {}` gives, in its order, each less its
/// `WIP_BODIES` fields and nothing else (its `head` key, claim and scalars stay); the filters
/// still pick the rows; and `rows {}` is the snapshot's rows, whole, as before.
#[test]
fn rows_as_heads_are_the_rows_less_their_text_and_rows_whole_are_unchanged() {
    let s = sqlite("row-heads");
    seeded(&s);
    let (whole, _) = s.snapshot().unwrap();
    let all = s.rows(&RowFilter::default()).unwrap();
    assert_eq!(all, whole.rows.values().cloned().collect::<Vec<_>>(), "rows {{}} is the snapshot's rows, whole");

    let heads = s.rows(&RowFilter { heads: true, ..Default::default() }).unwrap();
    assert_eq!(heads.iter().map(|r| &r.id).collect::<Vec<_>>(), all.iter().map(|r| &r.id).collect::<Vec<_>>());
    for (h, w) in heads.iter().zip(&all) {
        assert_eq!(h, &Row { fields: head_of(w.fields.clone()), ..w.clone() }, "{}: the row less its text", h.id.0);
    }
    let open = heads.iter().find(|r| r.id.0 == "open").unwrap();
    assert!(!open.fields.contains_key("body") && !open.fields.contains_key("prose"), "an open row is cut too");
    assert_eq!(open.fields["claim"], "{\"agent_id\":\"cpd-1\"}");
    let done_head = heads.iter().find(|r| r.id.0 == "done-head").unwrap();
    assert_eq!(done_head.fields["head"], "{\"id\":\"done-head\"}", "the head key stays");
    assert_eq!(done_head.fields["updated"], "2026-11-20");

    let doing = RowFilter { states: vec![RowState::Doing], heads: true, ..Default::default() };
    let picked: Vec<_> = s.rows(&doing).unwrap().into_iter().map(|r| r.id.0).collect();
    assert_eq!(picked, ["open", "open-Doing"], "heads cuts what each row carries, never which rows");
}

/// `lists {}` is the snapshot's lists whole; `lists {heads: true}` each less its text, its
/// groups whole, a finished list a seat sits on cut too (unlike `wip`'s cut).
#[test]
fn lists_whole_are_the_snapshots_and_as_heads_lose_their_text_alone() {
    let s = sqlite("list-heads");
    seeded(&s);
    let (whole, _) = s.snapshot().unwrap();
    assert_eq!(s.lists(&ListFilter::default()).unwrap(), whole.lists.values().cloned().collect::<Vec<_>>());
    let heads = s.lists(&ListFilter { heads: true }).unwrap();
    assert_eq!(heads.len(), whole.lists.len());
    for (h, w) in heads.iter().zip(whole.lists.values()) {
        assert_eq!(h, &List { fields: head_of(w.fields.clone()), ..w.clone() }, "{}", h.id.0);
        assert!(!h.fields.contains_key("body"));
    }
    assert_eq!(heads[2].fields, fields(&[("title", "read api"), ("status", "running")]));
    assert_eq!(heads[1].fields, fields(&[("status", "done"), (WIP_SEATED, "true")]));
}

/// A reading is kept beside the state: set, read back by name or all, replaced with a later
/// `at`; and neither the state nor the log moves for it.
#[test]
fn a_reading_is_kept_beside_the_state_and_writes_no_event() {
    let s = sqlite("readings");
    seeded(&s);
    let (before, head) = s.snapshot().unwrap();
    assert!(s.readings(&[]).unwrap().is_empty(), "none before the first write");

    s.set_reading("throttle", r#"{"mode":"auto"}"#.into()).unwrap();
    s.set_reading("spend", r#"{"lines":[]}"#.into()).unwrap();
    let all = s.readings(&[]).unwrap();
    assert_eq!(all.keys().map(String::as_str).collect::<Vec<_>>(), ["spend", "throttle"]);
    assert_eq!(all["throttle"].json, r#"{"mode":"auto"}"#);
    assert!(all["throttle"].at.0 > 0, "stamped by the store's clock");

    let named = s.readings(&["throttle".into(), "nothing".into()]).unwrap();
    assert_eq!(named.keys().map(String::as_str).collect::<Vec<_>>(), ["throttle"], "an unwritten name is absent");

    std::thread::sleep(std::time::Duration::from_millis(5));
    s.set_reading("throttle", r#"{"mode":"off"}"#.into()).unwrap();
    let again = &s.readings(&["throttle".into()]).unwrap()["throttle"];
    assert_eq!(again.json, r#"{"mode":"off"}"#);
    assert!(again.at > all["throttle"].at, "a write replaces the time too");

    assert_eq!(s.snapshot().unwrap(), (before, head), "no reading is in the state, and none wrote an event");
    assert_eq!(s.head().unwrap(), head);
}

/// The in-memory store gives the same heads and keeps readings alike.
#[test]
fn the_memory_store_cuts_heads_and_keeps_readings_alike() {
    let s = sqlite("heads-agree");
    let m = MemStore::with_domain(Arc::new(Mirror));
    seeded(&s);
    seeded(&m);
    for f in [RowFilter::default(), RowFilter { heads: true, ..Default::default() }] {
        assert_eq!(s.rows(&f).unwrap(), m.rows(&f).unwrap(), "{f:?}");
    }
    for f in [ListFilter::default(), ListFilter { heads: true }] {
        assert_eq!(s.lists(&f).unwrap(), m.lists(&f).unwrap(), "{f:?}");
    }
    for st in [&s as &dyn Store, &m] {
        st.set_reading("wip", "{}".into()).unwrap();
        let r = st.readings(&[]).unwrap();
        assert_eq!((r.len(), r["wip"].json.as_str()), (1, "{}"));
    }
}
