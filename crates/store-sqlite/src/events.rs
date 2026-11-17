//! `wait_for`: wakes on a commit by any handle in this process (a condvar shared by every handle
//! on one file) and polls SQLite's `data_version` every [`POLL`] for a commit by another process.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Condvar, Mutex, OnceLock, Weak};
use std::time::{Duration, Instant};

use wsp_store_contract::*;

use crate::apply::head_of;
use crate::{be, SqliteStore};

/// The longest a waiter sleeps before it looks at `data_version`.
pub(crate) const POLL: Duration = Duration::from_millis(50);

pub(crate) struct Wake {
    gen: Mutex<u64>,
    cv: Condvar,
}

impl Wake {
    /// After a COMMIT that wrote events.
    pub(crate) fn bump(&self) {
        *self.gen.lock().unwrap_or_else(|e| e.into_inner()) += 1;
        self.cv.notify_all();
    }

    fn gen(&self) -> u64 {
        *self.gen.lock().unwrap_or_else(|e| e.into_inner())
    }
}

type Registry = Mutex<HashMap<PathBuf, Weak<Wake>>>;

/// The wake every handle on `path` shares.
pub(crate) fn wake_for(path: &Path) -> Arc<Wake> {
    static REG: OnceLock<Registry> = OnceLock::new();
    let key = std::fs::canonicalize(path).unwrap_or_else(|_| path.to_path_buf());
    let mut reg = REG.get_or_init(Default::default).lock().unwrap_or_else(|e| e.into_inner());
    if let Some(w) = reg.get(&key).and_then(Weak::upgrade) {
        return w;
    }
    reg.retain(|_, w| w.strong_count() > 0);
    let w = Arc::new(Wake { gen: Mutex::new(0), cv: Condvar::new() });
    reg.insert(key, Arc::downgrade(&w));
    w
}

impl SqliteStore {
    pub(crate) fn bump(&self) {
        self.wake.bump();
    }

    pub(crate) fn wait_events(&self, after: EventId, timeout: Duration) -> Result<EventId> {
        let deadline = Instant::now() + timeout;
        let mut seen = self.wake.gen();
        let mut dv = self.with_conn(data_version)?;
        let mut head = self.with_conn(|c| head_of(c))?;
        loop {
            if head > after.0 {
                return Ok(EventId(head));
            }
            let left = deadline.saturating_duration_since(Instant::now());
            if left.is_zero() {
                return Ok(EventId(head));
            }
            {
                let g = self.wake.gen.lock().unwrap_or_else(|e| e.into_inner());
                if *g == seen {
                    let _ = self.wake.cv.wait_timeout(g, POLL.min(left));
                }
            }
            let now_gen = self.wake.gen();
            let now_dv = self.with_conn(data_version)?;
            if now_gen != seen || now_dv != dv {
                seen = now_gen;
                dv = now_dv;
                head = self.with_conn(|c| head_of(c))?;
            }
        }
    }
}

fn data_version(c: &rusqlite::Connection) -> Result<i64> {
    c.query_row("PRAGMA data_version", [], |r| r.get(0)).map_err(be)
}
