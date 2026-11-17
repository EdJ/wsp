//! The SQLite backend of the state store (docs/state-core.md sections 2.4 and 6): [`SqliteStore`],
//! a whole `Store` on one WAL file. No `rusqlite` type crosses this crate's surface (D1: the
//! contract names none, so a Postgres backend can replace this one); every failure of the file is
//! an `Error::Backend` with text.
//!
//! Modules: `schema` (the tables, the pragmas), `apply` (`Commands::apply` and the helpers every
//! write shares), `leases` (`Leases` and `Claims`), `reads` (`Reads`, `Events`, `Texts`).

mod apply;
mod leases;
mod reads;
mod schema;

use std::fmt::Display;
use std::path::{Path, PathBuf};
use std::sync::atomic::AtomicU8;
use std::sync::{Arc, Mutex, MutexGuard};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use rusqlite::{Connection, TransactionBehavior};
use wsp_store_contract::*;

pub(crate) const LIVE: u8 = 0;
pub(crate) const ARMED: u8 = 1;
pub(crate) const DEAD: u8 = 2;

/// Any failure of the file, as the contract's `Error::Backend`.
pub(crate) fn be(e: impl Display) -> Error {
    Error::Backend { reason: e.to_string() }
}

pub(crate) fn crashed() -> Error {
    Error::Backend { reason: "the handle is dead: it crashed mid-apply (test hook)".into() }
}

pub(crate) fn now() -> At {
    At(SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_millis() as u64).unwrap_or(0))
}

pub(crate) fn plus(at: At, ttl: Duration) -> At {
    At(at.0.saturating_add(ttl.as_millis() as u64))
}

/// A handle on a store file. Another handle (here or in another process) on the same path is
/// another connection to the same database; `BEGIN IMMEDIATE` is the mutex between them.
pub struct SqliteStore {
    /// `None` once the crash hook has fired: the handle is dead.
    conn: Mutex<Option<Connection>>,
    path: PathBuf,
    domain: Option<Arc<dyn Domain + Send + Sync>>,
    hook: AtomicU8,
}

impl std::fmt::Debug for SqliteStore {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("SqliteStore").field("path", &self.path).finish_non_exhaustive()
    }
}

impl SqliteStore {
    /// Opens (creating, if absent) the store file at `path`, whose `apply` validates through `domain`.
    pub fn open(path: impl AsRef<Path>, domain: Arc<dyn Domain + Send + Sync>) -> Result<Self> {
        let path = path.as_ref().to_path_buf();
        let conn = Connection::open(&path).map_err(be)?;
        schema::pragmas(&conn)?;
        schema::init(&conn)?;
        Ok(SqliteStore { conn: Mutex::new(Some(conn)), path, domain: Some(domain), hook: AtomicU8::new(LIVE) })
    }

    /// The file this handle is on.
    pub fn path(&self) -> &Path {
        &self.path
    }

    /// The domain `apply` validates through; `None` for a handle that has none.
    pub fn domain(&self) -> Option<Arc<dyn Domain + Send + Sync>> {
        self.domain.clone()
    }

    /// Arms the crash hook: the next `apply` through this handle stops midway, its transaction
    /// uncommitted, and the handle is dead after it.
    #[doc(hidden)]
    pub fn crash_next_apply(&self) {
        let _ = self.hook.compare_exchange(LIVE, ARMED, std::sync::atomic::Ordering::SeqCst, std::sync::atomic::Ordering::SeqCst);
    }

    fn lock(&self) -> MutexGuard<'_, Option<Connection>> {
        self.conn.lock().unwrap_or_else(|e| e.into_inner())
    }

    fn domain_or_err(&self) -> Result<&dyn Domain> {
        match &self.domain {
            Some(d) => Ok(&**d),
            None => Err(be("this handle has no domain: it is read-only")),
        }
    }

    /// Runs `f` on the connection; a dead handle is an `Error::Backend`.
    pub(crate) fn with_conn<T>(&self, f: impl FnOnce(&Connection) -> Result<T>) -> Result<T> {
        let g = self.lock();
        f(g.as_ref().ok_or_else(crashed)?)
    }

    /// Runs `f` in one `BEGIN IMMEDIATE ... COMMIT`; an `Err` rolls back.
    pub(crate) fn tx<T>(&self, f: impl FnOnce(&Connection) -> Result<T>) -> Result<T> {
        let mut g = self.lock();
        let conn = g.as_mut().ok_or_else(crashed)?;
        let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate).map_err(be)?;
        let out = f(&tx)?;
        tx.commit().map_err(be)?;
        Ok(out)
    }
}

/// The version of the SQLite this crate bundles, as SQLite reports it (`"3.46.0"`).
pub fn sqlite_version() -> String {
    rusqlite::version().to_string()
}

#[cfg(test)]
mod tests {
    use super::*;
    use rusqlite::Connection;

    fn open() -> (std::path::PathBuf, Connection) {
        let dir = std::env::temp_dir().join(format!("wsp-store-sqlite-{}-{:?}", std::process::id(), std::thread::current().id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let db = Connection::open(dir.join("state.db")).unwrap();
        (dir, db)
    }

    #[test]
    fn the_bundled_version_is_at_least_3_35() {
        let v = sqlite_version();
        let n: Vec<u32> = v.split('.').map(|p| p.parse().unwrap()).collect();
        assert!((n[0], n[1]) >= (3, 35), "bundled SQLite is {v}; RETURNING needs 3.35");
    }

    #[test]
    fn returning_works() {
        let (dir, db) = open();
        db.execute_batch("CREATE TABLE t (id INTEGER PRIMARY KEY, v TEXT)").unwrap();
        let id: i64 = db.query_row("INSERT INTO t (v) VALUES ('a') RETURNING id", [], |r| r.get(0)).unwrap();
        assert_eq!(id, 1);
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn json_extract_works() {
        let (dir, db) = open();
        let v: String = db.query_row(r#"SELECT json_extract('{"a":{"b":"x"}}', '$.a.b')"#, [], |r| r.get(0)).unwrap();
        assert_eq!(v, "x");
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn wal_sticks() {
        let (dir, db) = open();
        let mode: String = db.query_row("PRAGMA journal_mode=WAL", [], |r| r.get(0)).unwrap();
        assert_eq!(mode, "wal");
        drop(db);
        // WAL is a property of the file: a second connection sees it without asking.
        let again = Connection::open(dir.join("state.db")).unwrap();
        let mode: String = again.query_row("PRAGMA journal_mode", [], |r| r.get(0)).unwrap();
        assert_eq!(mode, "wal");
        let _ = std::fs::remove_dir_all(dir);
    }
}
