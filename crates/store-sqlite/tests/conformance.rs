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
