//! An event's workspace (wsp-171, the note from wsp-168's verifier): the events table keeps it, a
//! shared event reads back shared, and an event stored before the column existed reads private.

use std::path::PathBuf;
use std::sync::Arc;

use wsp_store_contract::*;
use wsp_store_sqlite::SqliteStore;

/// Puts each row under its own workspace, as the mirror domain does.
struct Put;

impl Domain for Put {
    fn decide(&self, _: &State, cmd: &Command, _: &Principal) -> std::result::Result<Vec<Draft>, Refusal> {
        let Command::Mirror { put, .. } = cmd else { return Ok(vec![]) };
        Ok(put
            .iter()
            .filter_map(|e| match e {
                Entity::Row(r) => Some(Draft {
                    kind: EventKind::MirrorPut,
                    subject: Subject::Row(r.id.clone()),
                    workspace: r.workspace.clone(),
                    payload: serde_json::to_value(e).unwrap(),
                }),
                _ => None,
            })
            .collect())
    }

    fn evolve(&self, s: State, _: &Event) -> State {
        s
    }
}

fn row(id: &str, workspace: WorkspaceId) -> Entity {
    Entity::Row(Row {
        workspace,
        id: RowId(id.into()),
        project: ProjectId("wsp".into()),
        title: id.into(),
        state: RowState::Todo,
        before_block: None,
        seat: None,
        group: None,
        commit: None,
        fields: Default::default(),
    })
}

fn path(tag: &str) -> PathBuf {
    let dir = PathBuf::from(env!("CARGO_TARGET_TMPDIR"));
    std::fs::create_dir_all(&dir).unwrap();
    let p = dir.join(format!("workspace-{tag}-{}.db", std::process::id()));
    for ext in ["", "-wal", "-shm"] {
        let _ = std::fs::remove_file(format!("{}{ext}", p.display()));
    }
    p
}

fn put(s: &SqliteStore, key: &str, es: Vec<Entity>) {
    s.apply(Command::Mirror { put: es, gone: vec![] }, IdempotencyKey(key.into()), Principal::Wsp("test".into())).unwrap();
}

#[test]
fn a_shared_event_reads_back_shared_and_a_private_one_private() {
    let p = path("round");
    let s = SqliteStore::open(&p, Arc::new(Put)).unwrap();
    let core = WorkspaceId::Shared(SharedName("core".into()));
    put(&s, "a", vec![row("r-1", core.clone()), row("r-2", WorkspaceId::Private)]);
    let read = |s: &SqliteStore| s.events_after(EventId(0), 10).unwrap().into_iter().map(|e| e.workspace).collect::<Vec<_>>();
    assert_eq!(read(&s), vec![core.clone(), WorkspaceId::Private]);
    // A new handle on the file reads the same.
    assert_eq!(read(&SqliteStore::open(&p, Arc::new(Put)).unwrap()), vec![core, WorkspaceId::Private]);
}

#[test]
fn an_event_stored_before_the_column_existed_reads_private() {
    let p = path("old");
    {
        // A schema-1 file: the events table as it was, one event in it.
        let c = rusqlite::Connection::open(&p).unwrap();
        c.execute_batch(
            "CREATE TABLE event (id INTEGER PRIMARY KEY, at INTEGER NOT NULL, principal TEXT NOT NULL,
                kind TEXT NOT NULL, subject TEXT NOT NULL, payload TEXT NOT NULL);
             PRAGMA user_version = 1;",
        )
        .unwrap();
        let (principal, kind, subject) = (
            serde_json::to_string(&Principal::Wsp("test".into())).unwrap(),
            serde_json::to_string(&EventKind::MirrorPut).unwrap(),
            serde_json::to_string(&Subject::Row(RowId("old".into()))).unwrap(),
        );
        c.execute(
            "INSERT INTO event (id, at, principal, kind, subject, payload) VALUES (1, 5, ?1, ?2, ?3, 'null')",
            [principal, kind, subject],
        )
        .unwrap();
    }
    let s = SqliteStore::open(&p, Arc::new(Put)).unwrap();
    let evs = s.events_after(EventId(0), 10).unwrap();
    assert_eq!(evs.len(), 1);
    assert_eq!(evs[0].workspace, WorkspaceId::Private);
    // And it takes a shared one after it.
    put(&s, "b", vec![row("new", WorkspaceId::Shared(SharedName("core".into())))]);
    let evs = s.events_after(EventId(0), 10).unwrap();
    assert_eq!((evs[0].workspace.clone(), evs[1].workspace.to_string()), (WorkspaceId::Private, "shared:core".to_string()));
}
