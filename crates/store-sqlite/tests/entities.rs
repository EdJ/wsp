//! Schema 3 (wsp-757): the folded state as one `entity` row per entity, not one blob. A schema-2
//! blob migrates to rows and reads back the same; a fold writes only the entities that changed;
//! a handle's cache of the state never hides another handle's write.

use std::collections::BTreeSet;
use std::path::PathBuf;
use std::sync::Arc;

use rusqlite::Connection;
use wsp_store_contract::*;
use wsp_store_sqlite::SqliteStore;

/// `mirror` alone: a put is a `mirror.put` of the entity, a gone a `mirror.gone` of the subject.
struct Mirror;

impl Domain for Mirror {
    fn decide(&self, _: &State, cmd: &Command, _: &Principal) -> std::result::Result<Vec<Draft>, Refusal> {
        let Command::Mirror { put, gone } = cmd else { return Ok(vec![]) };
        let draft = |kind, subject, payload| Draft { workspace: Default::default(), kind, subject, payload };
        Ok(put
            .iter()
            .map(|e| match e {
                Entity::Row(r) => draft(EventKind::MirrorPut, Subject::Row(r.id.clone()), serde_json::to_value(e).unwrap()),
                Entity::Seat(s) => draft(EventKind::MirrorPut, Subject::Seat(s.id.clone()), serde_json::to_value(e).unwrap()),
                _ => unimplemented!("rows and seats are enough here"),
            })
            .chain(gone.iter().map(|s| draft(EventKind::MirrorGone, s.clone(), serde_json::Value::Null)))
            .collect())
    }

    fn evolve(&self, mut s: State, ev: &Event) -> State {
        match (&ev.kind, &ev.subject) {
            (EventKind::MirrorPut, _) => match serde_json::from_value::<Entity>(ev.payload.clone()).unwrap() {
                Entity::Row(r) => drop(s.rows.insert(r.id.clone(), r)),
                Entity::Seat(x) => drop(s.seats.insert(x.id.clone(), x)),
                _ => {}
            },
            (EventKind::MirrorGone, Subject::Row(id)) => drop(s.rows.remove(id)),
            _ => {}
        }
        s
    }
}

fn path(tag: &str) -> PathBuf {
    let dir = PathBuf::from(env!("CARGO_TARGET_TMPDIR"));
    std::fs::create_dir_all(&dir).unwrap();
    let p = dir.join(format!("entities-{tag}-{}.db", std::process::id()));
    for ext in ["", "-wal", "-shm"] {
        let _ = std::fs::remove_file(format!("{}{ext}", p.display()));
    }
    p
}

fn row(id: &str, title: &str) -> Row {
    Row {
        workspace: Default::default(),
        id: RowId(id.into()),
        project: ProjectId("wsp".into()),
        title: title.into(),
        state: RowState::Todo,
        before_block: None,
        seat: None,
        group: None,
        commit: None,
        fields: [("body".to_string(), format!("{id}'s body"))].into(),
    }
}

fn seat(id: &str) -> Seat {
    Seat {
        id: SeatId(id.into()),
        machine: MachineName("mac".into()),
        kind: "claude".into(),
        model: None,
        state: SeatState::Idle,
        before_lost: None,
        row: None,
        attempt: 0,
        pid: None,
        fields: Default::default(),
    }
}

fn apply(s: &SqliteStore, key: &str, put: Vec<Entity>, gone: Vec<Subject>) {
    s.apply(Command::Mirror { put, gone }, IdempotencyKey(key.into()), Principal::Wsp("test".into())).unwrap();
}

#[test]
fn a_schema_2_blob_migrates_to_entity_rows_and_reads_back_the_same() {
    let p = path("migrate");
    let mut before = State::default();
    for id in ["wsp-1", "wsp-2", "a key with \"quotes\""] {
        before.rows.insert(RowId(id.into()), row(id, "t"));
    }
    before.seats.insert(SeatId("cpd-1".into()), seat("cpd-1"));
    let key = RunKey { row: RowId("wsp-1".into()), attempt: 2, kind: RunKind::Check };
    before.runs = vec![Run { key: key.clone(), state: RunState::Running, verdict: None, log: None }, Run { key, state: RunState::Abandoned, verdict: None, log: Some("l".into()) }];
    before.members.insert(SharedName("core".into()), BTreeSet::from([UserName("ed".into())]));
    {
        // A schema-2 file as the last build left it: the state one blob, folded through event 7.
        let c = Connection::open(&p).unwrap();
        c.execute_batch(
            "CREATE TABLE event (id INTEGER PRIMARY KEY, at INTEGER NOT NULL, principal TEXT NOT NULL, kind TEXT NOT NULL,
                 subject TEXT NOT NULL, payload TEXT NOT NULL, workspace TEXT NOT NULL DEFAULT 'private');
             CREATE TABLE state (id INTEGER PRIMARY KEY CHECK (id = 1), json TEXT NOT NULL, at INTEGER NOT NULL);
             PRAGMA user_version = 2;",
        )
        .unwrap();
        c.execute("INSERT INTO state (id, json, at) VALUES (1, ?1, 7)", [serde_json::to_string(&before).unwrap()]).unwrap();
    }
    let s = SqliteStore::open(&p, Arc::new(Mirror)).unwrap();
    assert_eq!(s.snapshot().unwrap().0, before);
    assert_eq!(s.row(&RowId("a key with \"quotes\"".into())).unwrap(), before.rows.get(&RowId("a key with \"quotes\"".into())).cloned());
    let c = Connection::open(&p).unwrap();
    let version: i64 = c.query_row("PRAGMA user_version", [], |r| r.get(0)).unwrap();
    assert_eq!(version, 3);
    let blob: bool = c.prepare("SELECT 1 FROM sqlite_master WHERE name = 'state'").unwrap().exists([]).unwrap();
    assert!(!blob, "the blob is dropped once its entities are rows");
    let at: i64 = c.query_row("SELECT at FROM fold", [], |r| r.get(0)).unwrap();
    assert_eq!(at, 7);

    // It folds on from there, and a new handle reads what this one wrote.
    apply(&s, "a", vec![Entity::Row(row("wsp-3", "new"))], vec![Subject::Row(RowId("wsp-2".into()))]);
    let mut after = before.clone();
    after.rows.insert(RowId("wsp-3".into()), row("wsp-3", "new"));
    after.rows.remove(&RowId("wsp-2".into()));
    assert_eq!(s.snapshot().unwrap().0, after);
    assert_eq!(SqliteStore::open(&p, Arc::new(Mirror)).unwrap().snapshot().unwrap().0, after);
}

#[test]
fn a_fold_writes_only_the_entities_that_changed() {
    let p = path("diff");
    let s = SqliteStore::open(&p, Arc::new(Mirror)).unwrap();
    let rows: Vec<Entity> = (0..50).map(|i| Entity::Row(row(&format!("wsp-{i}"), "t"))).collect();
    apply(&s, "seed", rows, vec![]);
    let c = Connection::open(&p).unwrap();
    c.execute_batch(
        "CREATE TABLE wrote (op TEXT, map TEXT, key TEXT);
         CREATE TRIGGER wrote_i AFTER INSERT ON entity BEGIN INSERT INTO wrote VALUES ('put', new.map, new.key); END;
         CREATE TRIGGER wrote_u AFTER UPDATE ON entity BEGIN INSERT INTO wrote VALUES ('put', new.map, new.key); END;
         CREATE TRIGGER wrote_d AFTER DELETE ON entity BEGIN INSERT INTO wrote VALUES ('gone', old.map, old.key); END;",
    )
    .unwrap();
    // One row the same as it was, one changed, one seat new, one row gone.
    apply(
        &s,
        "one",
        vec![Entity::Row(row("wsp-1", "t")), Entity::Row(row("wsp-2", "changed")), Entity::Seat(seat("cpd-9"))],
        vec![Subject::Row(RowId("wsp-3".into()))],
    );
    let mut stmt = c.prepare("SELECT op, map, key FROM wrote ORDER BY op, map, key").unwrap();
    let wrote: Vec<(String, String, String)> =
        stmt.query_map([], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?))).unwrap().map(|r| r.unwrap()).collect();
    let want = |op: &str, map: &str, key: &str| (op.to_string(), map.to_string(), key.to_string());
    assert_eq!(wrote, vec![want("gone", "rows", "wsp-3"), want("put", "rows", "wsp-2"), want("put", "seats", "cpd-9")]);
}

#[test]
fn a_handles_cached_state_never_hides_another_handles_write() {
    let p = path("cache");
    let a = SqliteStore::open(&p, Arc::new(Mirror)).unwrap();
    let b = SqliteStore::open(&p, Arc::new(Mirror)).unwrap();
    apply(&a, "a1", vec![Entity::Row(row("wsp-1", "a"))], vec![]);
    assert_eq!(a.snapshot().unwrap().0.rows.len(), 1); // a's cache is now of that fold
    apply(&b, "b1", vec![Entity::Row(row("wsp-2", "b"))], vec![]);
    // a reads b's write, and folds its own on top of it, not on its cache.
    assert_eq!(a.snapshot().unwrap().0.rows.len(), 2);
    apply(&b, "b2", vec![Entity::Row(row("wsp-3", "b"))], vec![]);
    apply(&a, "a2", vec![Entity::Row(row("wsp-1", "a again"))], vec![]);
    let fresh = SqliteStore::open(&p, Arc::new(Mirror)).unwrap().snapshot().unwrap();
    assert_eq!(fresh.0.rows.len(), 3);
    assert_eq!(fresh.0.rows[&RowId("wsp-1".into())].title, "a again");
    assert_eq!(a.snapshot().unwrap(), fresh);
    assert_eq!(b.snapshot().unwrap(), fresh);
    assert_eq!(b.seats(&SeatFilter::default()).unwrap(), vec![]);
    assert_eq!(b.rows(&RowFilter::default()).unwrap().len(), 3);
}

#[test]
fn compact_drops_superseded_mirror_puts_and_truncates_the_wal() {
    let p = path("compact");
    let s = SqliteStore::open(&p, Arc::new(Mirror)).unwrap();
    for i in 0..5 {
        apply(&s, &format!("a{i}"), vec![Entity::Row(row("wsp-1", &format!("t{i}")))], vec![]);
    }
    apply(&s, "b", vec![Entity::Row(row("wsp-2", "x"))], vec![]);
    apply(&s, "c", vec![Entity::Row(row("wsp-3", "y"))], vec![]);
    apply(&s, "d", vec![], vec![Subject::Row(RowId("wsp-3".into()))]);
    let (state, head) = (s.snapshot().unwrap().0, s.head().unwrap());
    let c = s.compact().unwrap();
    // wsp-1's first four puts, and wsp-3's put (its gone supersedes it); the last put of wsp-1, wsp-2's, and the gone stay.
    assert_eq!(c.deleted, 5);
    assert_eq!(c.wal_bytes, 0);
    assert_eq!(s.head().unwrap(), head);
    assert_eq!(s.snapshot().unwrap().0, state);
    assert_eq!(s.events_after(EventId(0), 100).unwrap().len(), 3);
    assert_eq!(s.compact().unwrap().deleted, 0);
}
