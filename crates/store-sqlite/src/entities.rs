//! The folded state, an `entity` row per entity (schema 3, wsp-757), and the handle's cache of it.
//!
//! Until schema 3 the state was one JSON blob, a row of `state`: every `apply` read, parsed, folded,
//! serialised and wrote back all of it (55 MB on the live store, ~450 ms and 55 MB of WAL an apply),
//! and every read parsed it whole. Now each entity of a [`State`] map is a row of
//! `entity (map, key, json)`, `map` naming the field (`rows`, `seats`, ...) and `key` its key as the
//! blob's JSON had it; `runs`, a `Vec`, is one row of key `''`. `fold.at` is the id of the last event
//! folded.
//!
//! A fold writes only the entities that changed (by `PartialEq` against the state before it). The
//! handle keeps the state it last read or wrote, with the `at` it is of: a later call whose
//! `fold.at` is the same uses it unparsed. Every write folds a new event, so a new `at` is a new
//! state, whichever process wrote it.

use std::sync::{Mutex, MutexGuard};

use rusqlite::{params, Connection, OptionalExtension};
use serde_json::Value;
use wsp_store_contract::*;

use crate::apply::{from_json, to_json};
use crate::be;

/// The state as of `at` (the id of the last event folded into it).
pub(crate) struct Folded {
    pub(crate) at: i64,
    pub(crate) state: State,
}

/// What a handle last read or wrote; `None` until the first read, and after a failed write.
#[derive(Default)]
pub(crate) struct Cache(Mutex<Option<Folded>>);

impl Cache {
    pub(crate) fn lock(&self) -> MutexGuard<'_, Option<Folded>> {
        self.0.lock().unwrap_or_else(|e| e.into_inner())
    }

    /// Forgets the state: a write that failed after its fold leaves it unknown.
    pub(crate) fn clear(&self) {
        *self.lock() = None;
    }
}

/// The text a map's key is stored under: the key as `serde_json` writes it, unquoted.
macro_rules! key_text {
    ($k:expr) => {
        match serde_json::to_value($k).map_err($crate::be)? {
            serde_json::Value::String(s) => s,
            v => v.to_string(),
        }
    };
}

pub(crate) use key_text;

/// Calls `$m!(field, ...)` with every map of [`State`] (all but `runs`). [`write_diff`]
/// destructures `State` whole, so a field added there and missed here does not compile.
macro_rules! maps {
    ($m:ident) => {
        $m!(rows, lists, seats, claims, orders, asks, machines, leases, users, credentials, workspaces, members)
    };
}

/// The id of the last event folded, `0` before the first.
pub(crate) fn folded_at(c: &Connection) -> Result<i64> {
    let at: Option<i64> = c.query_row("SELECT at FROM fold WHERE id = 1", [], |r| r.get(0)).optional().map_err(be)?;
    Ok(at.unwrap_or(0))
}

/// The whole state, from the `entity` table.
pub(crate) fn load(c: &Connection) -> Result<State> {
    let mut s = State::default();
    let mut stmt = c.prepare_cached("SELECT map, key, json FROM entity").map_err(be)?;
    let mut rows = stmt.query([]).map_err(be)?;
    while let Some(r) = rows.next().map_err(be)? {
        let (map, key, json): (String, String, String) = (r.get(0).map_err(be)?, r.get(1).map_err(be)?, r.get(2).map_err(be)?);
        macro_rules! put {
            ($($f:ident),*) => {
                match map.as_str() {
                    $(stringify!($f) => {
                        s.$f.insert(serde_json::from_value(Value::String(key)).map_err(be)?, from_json!(&json)?);
                    })*
                    "runs" => s.runs = from_json!(&json)?,
                    other => return Err(be(format!("the entity table holds a map this build does not know: {other}"))),
                }
            };
        }
        maps!(put);
    }
    Ok(s)
}

/// Runs `f` on the state as of `fold.at`: the cached one when it is of that `at`, else read anew
/// (and cached). The caller holds the connection, so nothing commits between the two reads it
/// makes unless it runs outside a transaction; [`crate::reads`] wraps its reads in one.
pub(crate) fn with_state<T>(c: &Connection, cache: &Cache, f: impl FnOnce(&State) -> T) -> Result<T> {
    let at = folded_at(c)?;
    let mut g = cache.lock();
    if g.as_ref().is_none_or(|h| h.at != at) {
        *g = Some(Folded { at, state: load(c)? });
    }
    Ok(f(&g.as_ref().expect("just filled").state))
}

/// Writes to `entity` what differs between `old` and `new`: a changed or new entity is put, a
/// gone one deleted. An unchanged entity is not touched.
pub(crate) fn write_diff(c: &Connection, old: &State, new: &State) -> Result<()> {
    // Whole, with no `..`: a field added to `State` must be added to `maps!` too.
    let State { rows: _, lists: _, seats: _, claims: _, orders: _, asks: _, runs: _, machines: _, leases: _, users: _, credentials: _, workspaces: _, members: _ } = new;
    let mut up = c
        .prepare_cached("INSERT INTO entity (map, key, json) VALUES (?1, ?2, ?3) ON CONFLICT(map, key) DO UPDATE SET json = excluded.json")
        .map_err(be)?;
    let mut del = c.prepare_cached("DELETE FROM entity WHERE map = ?1 AND key = ?2").map_err(be)?;
    macro_rules! diff {
        ($($f:ident),*) => {$(
            for (k, v) in &new.$f {
                if old.$f.get(k) != Some(v) {
                    up.execute(params![stringify!($f), key_text!(k), to_json!(v)?]).map_err(be)?;
                }
            }
            for k in old.$f.keys() {
                if !new.$f.contains_key(k) {
                    del.execute(params![stringify!($f), key_text!(k)]).map_err(be)?;
                }
            }
        )*};
    }
    maps!(diff);
    if old.runs != new.runs {
        up.execute(params!["runs", "", to_json!(&new.runs)?]).map_err(be)?;
    }
    Ok(())
}

/// Folds `events` (in a write transaction) into the state, writes what changed and the new `at`,
/// and leaves the cache holding the result. A caller whose transaction then fails clears it.
pub(crate) fn fold(c: &Connection, cache: &Cache, domain: &dyn Domain, events: &[Event]) -> Result<()> {
    let Some(last) = events.last() else { return Ok(()) };
    let at = folded_at(c)?;
    let mut g = cache.lock();
    if g.as_ref().is_none_or(|h| h.at != at) {
        *g = Some(Folded { at, state: load(c)? });
    }
    let old = &g.as_ref().expect("just filled").state;
    let new = events.iter().fold(old.clone(), |s, e| domain.evolve(s, e));
    write_diff(c, old, &new)?;
    let at = last.id.0 as i64;
    c.execute("INSERT INTO fold (id, at) VALUES (1, ?1) ON CONFLICT(id) DO UPDATE SET at = excluded.at", [at]).map_err(be)?;
    *g = Some(Folded { at, state: new });
    Ok(())
}

/// One entity's JSON, read alone: no other entity is decoded.
pub(crate) fn one<T>(c: &Connection, map: &str, key: &str, decode: impl FnOnce(&str) -> Result<T>) -> Result<Option<T>> {
    let json: Option<String> = c
        .prepare_cached("SELECT json FROM entity WHERE map = ?1 AND key = ?2")
        .and_then(|mut s| s.query_row(params![map, key], |r| r.get(0)).optional())
        .map_err(be)?;
    json.map(|j| decode(&j)).transpose()
}
