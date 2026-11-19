//! 2.3's conformance cases against `SqliteStore`, one `#[test]` each, on real files.

use std::path::PathBuf;
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::Arc;

use wsp_store_conformance::cases::*;
use wsp_store_conformance::Harness;
use wsp_store_contract::Domain;
use wsp_store_sqlite::SqliteStore;

struct SqliteHarness;

static NEXT: AtomicU32 = AtomicU32::new(0);

impl Harness for SqliteHarness {
    type Handle = SqliteStore;

    fn create(&self, domain: Arc<dyn Domain + Send + Sync>) -> SqliteStore {
        let dir = PathBuf::from(env!("CARGO_TARGET_TMPDIR"));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join(format!("store-{}-{}.db", std::process::id(), NEXT.fetch_add(1, Ordering::SeqCst)));
        SqliteStore::open(path, domain).unwrap()
    }

    fn open(&self, on: &SqliteStore) -> SqliteStore {
        SqliteStore::open(on.path(), on.domain().unwrap()).unwrap()
    }

    fn crash_next_apply(&self, h: &SqliteStore) {
        h.crash_next_apply();
    }

    fn reopen(&self, h: &SqliteStore) -> SqliteStore {
        SqliteStore::open(h.path(), h.domain().unwrap()).unwrap()
    }
}

#[test]
fn case_1_atomicity() {
    case_atomicity(&SqliteHarness).unwrap();
}

#[test]
fn case_2_idempotency() {
    case_idempotency(&SqliteHarness).unwrap();
}

#[test]
fn case_4_fencing() {
    case_fencing(&SqliteHarness).unwrap();
}

#[test]
fn case_6_snapshot() {
    case_snapshot(&SqliteHarness).unwrap();
}

#[test]
fn case_9_machine_guards() {
    case_machine_guards(&SqliteHarness).unwrap();
}

#[test]
fn case_3_lease_exclusivity() {
    case_lease_exclusivity(&SqliteHarness).unwrap();
}

#[test]
fn case_10_text() {
    case_text(&SqliteHarness).unwrap();
}

/// D1: no `rusqlite` type crosses the crate's surface. Any `pub` line of src naming `rusqlite` or
/// `Connection` fails.
#[test]
fn no_pub_line_names_rusqlite_or_connection() {
    let mut bad = vec![];
    let dir = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("src");
    for e in std::fs::read_dir(&dir).unwrap() {
        let path = e.unwrap().path();
        if path.extension().is_none_or(|x| x != "rs") {
            continue;
        }
        for (n, line) in std::fs::read_to_string(&path).unwrap().lines().enumerate() {
            let t = line.trim_start();
            if t.starts_with("//") {
                continue;
            }
            if t.starts_with("pub ") && (t.contains("rusqlite") || t.contains("Connection")) {
                bad.push(format!("{}:{}: {}", path.display(), n + 1, t));
            }
        }
    }
    assert!(bad.is_empty(), "rusqlite types on the public surface (D1):\n{}", bad.join("\n"));
}

#[test]
fn case_5_event_order() {
    case_event_order(&SqliteHarness).unwrap();
}

#[test]
fn case_7_wakeups() {
    case_wakeups(&SqliteHarness).unwrap();
}

#[test]
fn case_8_crash() {
    case_crash(&SqliteHarness).unwrap();
}

#[test]
fn a_read_only_handle_reads_while_another_applies_and_is_refused_itself() {
    use wsp_store_contract::*;
    let w = SqliteHarness.fresh();
    let ro = SqliteStore::open_read_only(w.path()).unwrap();
    let writer = std::thread::spawn(move || {
        for i in 0..20 {
            let cmd = Command::RowAdd { project: ProjectId("ro".into()), title: format!("t{i}"), fields: Default::default() };
            w.apply(cmd, IdempotencyKey(format!("ro-{i}")), Principal::Wsp("test".into())).unwrap();
        }
    });
    let mut last = 0;
    while !writer.is_finished() {
        let (state, at) = ro.snapshot().unwrap();
        assert!(at.0 >= last);
        last = at.0;
        assert!(state.rows.len() as u64 <= at.0);
        assert!(ro.events_after(EventId(0), usize::MAX).unwrap().len() as u64 >= at.0);
    }
    writer.join().unwrap();
    assert_eq!(ro.head().unwrap().0, 20);
    assert_eq!(ro.events_after(EventId(0), usize::MAX).unwrap().len(), 20);
    assert_eq!(ro.wait_for(EventId(5), std::time::Duration::from_millis(10)).unwrap().0, 20);
    let cmd = Command::RowAdd { project: ProjectId("ro".into()), title: "x".into(), fields: Default::default() };
    match ro.apply(cmd, IdempotencyKey("ro-refused".into()), Principal::Wsp("test".into())) {
        Err(Error::Backend { reason }) => assert!(reason.contains("read-only"), "{reason}"),
        other => panic!("a read-only apply answered {other:?}"),
    }
}
