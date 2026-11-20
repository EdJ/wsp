//! The file's schema (docs/state-core.md 2.4). Every JSON column is `serde_json` of the contract type
//! it names, so a reader needs no rusqlite type and a Postgres backend can keep the same shapes.

use std::time::Duration;

use rusqlite::Connection;

use crate::be;
use wsp_store_contract::Result;

/// The schema version this build writes and reads (`PRAGMA user_version`).
pub(crate) const VERSION: i64 = 1;

const DDL: &str = "
CREATE TABLE IF NOT EXISTS event (
    id        INTEGER PRIMARY KEY,
    at        INTEGER NOT NULL,
    principal TEXT NOT NULL,
    kind      TEXT NOT NULL,
    subject   TEXT NOT NULL,
    payload   TEXT NOT NULL
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
CREATE TABLE IF NOT EXISTS state (
    id   INTEGER PRIMARY KEY CHECK (id = 1),
    json TEXT NOT NULL,
    at   INTEGER NOT NULL
);
CREATE TABLE IF NOT EXISTS text (
    row     TEXT NOT NULL,
    section TEXT NOT NULL,
    body    TEXT NOT NULL,
    rev     INTEGER NOT NULL,
    PRIMARY KEY (row, section)
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
    if version < VERSION {
        conn.pragma_update(None, "user_version", VERSION).map_err(be)?;
    }
    Ok(())
}
