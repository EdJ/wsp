//! `SqliteStore::compact`: drops the `mirror.put` events a later `mirror.put` or `mirror.gone` of the
//! same subject has superseded (the folded state already holds only the last), then checkpoints
//! the WAL and truncates it. The head and every other kind of event stay, so ids stay sparse but
//! `head`, `events_after` and `wait_for` read as before.

use rusqlite::Connection;
use wsp_store_contract::*;

use crate::{be, SqliteStore};

/// What one `compact` did.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Compacted {
    /// Superseded `mirror.put` events deleted.
    pub deleted: u64,
    /// Bytes the WAL file holds after the checkpoint (0 when it was truncated).
    pub wal_bytes: u64,
}

impl SqliteStore {
    /// Deletes superseded `mirror.put` history and checkpoints (truncating) the WAL.
    pub fn compact(&self) -> Result<Compacted> {
        let deleted = self.tx(|c| {
            let n = c
                .execute(
                    "DELETE FROM event WHERE kind = '\"mirror.put\"' AND id NOT IN (
                         SELECT MAX(id) FROM event
                         WHERE kind IN ('\"mirror.put\"', '\"mirror.gone\"') GROUP BY subject)",
                    [],
                )
                .map_err(be)?;
            Ok(n as u64)
        })?;
        self.with_conn(|c| checkpoint(c))?;
        let mut wal = self.path.clone().into_os_string();
        wal.push("-wal");
        let wal_bytes = std::fs::metadata(wal).map(|m| m.len()).unwrap_or(0);
        Ok(Compacted { deleted, wal_bytes })
    }
}

fn checkpoint(c: &Connection) -> Result<()> {
    c.query_row("PRAGMA wal_checkpoint(TRUNCATE)", [], |_| Ok(())).map_err(be)
}
