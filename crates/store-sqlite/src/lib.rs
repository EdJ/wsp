//! The SQLite backend of the state store (docs/state-core.md sections 2.4 and 6).
//!
//! Step 0 holds nothing but the bundled SQLite: no schema, no `Store`, no workspace dependency
//! (the contract joins at step 1). No `rusqlite` type crosses this crate's surface (D1: the
//! contract names none, so a Postgres backend can replace this one).

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
