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
