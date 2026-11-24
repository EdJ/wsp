//! The file's schema (docs/state-core.md 2.4). Every JSON column is `serde_json` of the contract type
//! it names, so a reader needs no rusqlite type and a Postgres backend can keep the same shapes.

use std::time::Duration;

use rusqlite::Connection;

use crate::be;
use wsp_store_contract::Result;

/// The schema version this build writes and reads (`PRAGMA user_version`).
pub(crate) const VERSION: i64 = 3;

const DDL: &str = "
CREATE TABLE IF NOT EXISTS event (
    id        INTEGER PRIMARY KEY,
    at        INTEGER NOT NULL,
    principal TEXT NOT NULL,
    kind      TEXT NOT NULL,
    subject   TEXT NOT NULL,
    payload   TEXT NOT NULL,
    workspace TEXT NOT NULL DEFAULT 'private'
);
CREATE TABLE IF NOT EXISTS command (
    key    TEXT PRIMARY KEY,
    result TEXT NOT NULL
);
CREATE TABLE IF NOT EXISTS lease (
    name    TEXT PRIMARY KEY,
    last    INTEGER NOT NULL,
    holder  TEXT,
    token   INTEGER,
    expires INTEGER
);
CREATE TABLE IF NOT EXISTS entity (
    map  TEXT NOT NULL,
    key  TEXT NOT NULL,
    json TEXT NOT NULL,
    PRIMARY KEY (map, key)
) WITHOUT ROWID;
CREATE TABLE IF NOT EXISTS fold (
    id INTEGER PRIMARY KEY CHECK (id = 1),
    at INTEGER NOT NULL
);
CREATE TABLE IF NOT EXISTS text (
    row     TEXT NOT NULL,
    section TEXT NOT NULL,
    body    TEXT NOT NULL,
    rev     INTEGER NOT NULL,
    PRIMARY KEY (row, section)
);
CREATE TABLE IF NOT EXISTS reading (
    name TEXT PRIMARY KEY,
    json TEXT NOT NULL,
    at   INTEGER NOT NULL
);
";

/// Opens the pragmas every handle runs under: a busy wait of 10 s (so a second writer's
/// `BEGIN IMMEDIATE` queues behind the first), WAL, and `synchronous=FULL` (a commit is on disk).
pub(crate) fn pragmas(conn: &Connection) -> Result<()> {
    conn.busy_timeout(Duration::from_secs(10)).map_err(be)?;
    conn.pragma_update_and_check(None, "journal_mode", "WAL", |r| r.get::<_, String>(0)).map_err(be)?;
    conn.pragma_update(None, "synchronous", "FULL").map_err(be)?;
    Ok(())
}

/// Creates the tables if the file has none, and stamps the version; refuses a file of a newer one.
pub(crate) fn init(conn: &Connection) -> Result<()> {
    let version: i64 = conn.query_row("PRAGMA user_version", [], |r| r.get(0)).map_err(be)?;
    if version > VERSION {
        return Err(be(format!("the store file is schema {version}; this build reads {VERSION}")));
    }
    conn.execute_batch(DDL).map_err(be)?;
    // A file from schema 1 has no workspace column: add it, and what it holds reads private.
    let has: bool = conn
        .prepare("SELECT 1 FROM pragma_table_info('event') WHERE name = 'workspace'")
        .and_then(|mut s| s.exists([]))
        .map_err(be)?;
    if !has {
        conn.execute_batch("ALTER TABLE event ADD COLUMN workspace TEXT NOT NULL DEFAULT 'private'").map_err(be)?;
    }
    if version < VERSION {
        migrate_state(conn)?;
    }
    Ok(())
}

/// Schema 2's one `state` blob, split into `entity` rows (schema 3, wsp-757), in one transaction
/// so a second process opening the file at once sees the blob or the rows, never half of either;
/// the version is stamped in it too.
/// Each map's entities keep the blob's keys and JSON, `runs` (an array) is one row of key `''`,
/// and `fold.at` is the blob's `at`; the blob is then dropped.
fn migrate_state(conn: &Connection) -> Result<()> {
    conn.execute_batch("BEGIN IMMEDIATE").map_err(be)?;
    let run = || -> rusqlite::Result<()> {
        let has: bool = conn.prepare("SELECT 1 FROM sqlite_master WHERE type = 'table' AND name = 'state'")?.exists([])?;
        if has {
            conn.execute_batch(
                "INSERT INTO entity (map, key, json)
                     SELECT m.key, e.key, json(e.value)
                     FROM state AS s, json_each(s.json) AS m, json_each(m.value) AS e
                     WHERE s.id = 1 AND m.key != 'runs' AND m.type = 'object'
                 ON CONFLICT(map, key) DO UPDATE SET json = excluded.json;
                 INSERT INTO entity (map, key, json)
                     SELECT 'runs', '', json(json_extract(s.json, '$.runs')) FROM state AS s
                     WHERE s.id = 1 AND json_type(s.json, '$.runs') = 'array'
                 ON CONFLICT(map, key) DO UPDATE SET json = excluded.json;
                 INSERT INTO fold (id, at) SELECT 1, at FROM state WHERE id = 1
                 ON CONFLICT(id) DO UPDATE SET at = excluded.at;
                 DROP TABLE state;",
            )?;
        }
        conn.pragma_update(None, "user_version", VERSION)
    };
    match run() {
        Ok(()) => conn.execute_batch("COMMIT").map_err(be),
        Err(e) => {
            let _ = conn.execute_batch("ROLLBACK");
            Err(be(e))
        }
    }
}
