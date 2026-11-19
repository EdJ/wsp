//! `Commands::apply`, and the helpers every write shares: the state row, stamping, and publishing.
//!
//! One `apply` is one `BEGIN IMMEDIATE ... COMMIT`, in this order: the key, the fence, `decide` on
//! the stored state, the events stamped `MAX(id)+1..` and inserted, the state folded by `evolve`
//! and written back, the key recorded. A refusal rolls back and records no key.

use std::sync::atomic::Ordering;

use rusqlite::{params, Connection, OptionalExtension};
use wsp_store_contract::*;

use crate::{be, crashed, now, SqliteStore, ARMED, DEAD};

/// The fence a command is issued under, if any (2.2); the contract names no accessor.
pub(crate) fn fence_of(cmd: &Command) -> Option<&Fence> {
    match cmd {
        Command::RowClaim { fence, .. } | Command::RowReview { fence, .. } | Command::RowLand { fence, .. } => {
            Some(fence)
        }
        Command::SeatEnd { fence, .. } => fence.as_ref(),
        _ => None,
    }
}

/// `serde_json` of a contract type, a failure as `Error::Backend`. Macros, not generic fns: this
/// crate names no `serde` of its own (its only dependencies are the contract and `serde_json`).
macro_rules! to_json {
    ($v:expr) => {
        serde_json::to_string($v).map_err($crate::be)
    };
}

macro_rules! from_json {
    ($s:expr) => {
        serde_json::from_str($s).map_err($crate::be)
    };
}

pub(crate) use {from_json, to_json};

/// The folded state as the state row holds it; empty before the first event.
pub(crate) fn load_state(c: &Connection) -> Result<State> {
    let row: Option<String> = c.query_row("SELECT json FROM state WHERE id = 1", [], |r| r.get(0)).optional().map_err(be)?;
    match row {
        Some(j) => from_json!(&j),
        None => Ok(State::default()),
    }
}

/// The last id committed, `0` when there is none.
pub(crate) fn head_of(c: &Connection) -> Result<u64> {
    let h: i64 = c.query_row("SELECT COALESCE(MAX(id), 0) FROM event", [], |r| r.get(0)).map_err(be)?;
    Ok(h as u64)
}

/// `stale` unless `fence` names its lease's current token, held.
pub(crate) fn check_fence(c: &Connection, fence: &Fence) -> Result<()> {
    let slot: Option<(i64, Option<i64>)> = c
        .query_row("SELECT last, token FROM lease WHERE name = ?1", [&fence.lease.0], |r| Ok((r.get(0)?, r.get(1)?)))
        .optional()
        .map_err(be)?;
    let (last, held) = slot.unwrap_or((0, None));
    match held {
        Some(t) if t as u64 == fence.token.0 => Ok(()),
        _ => Err(Error::Refused(Refusal::Stale { lease: fence.lease.clone(), current: Token(last as u64) })),
    }
}

/// Stamps `drafts` with the next ids, `at` and `by`, and inserts them.
pub(crate) fn insert_drafts(c: &Connection, drafts: Vec<Draft>, by: &Principal, at: At) -> Result<Vec<Event>> {
    let mut next = head_of(c)?;
    let mut stmt = c
        .prepare_cached("INSERT INTO event (id, at, principal, kind, subject, payload) VALUES (?1, ?2, ?3, ?4, ?5, ?6)")
        .map_err(be)?;
    let mut out = Vec::with_capacity(drafts.len());
    for d in drafts {
        next += 1;
        let e = Event { id: EventId(next), at, principal: by.clone(), kind: d.kind, subject: d.subject, payload: d.payload };
        stmt.execute(params![next as i64, at.0 as i64, to_json!(&e.principal)?, to_json!(&e.kind)?, to_json!(&e.subject)?, to_json!(&e.payload)?])
            .map_err(be)?;
        out.push(e);
    }
    Ok(out)
}

/// Folds `events` into the state row (`at` is the id of the last one folded).
pub(crate) fn fold(c: &Connection, domain: &dyn Domain, events: &[Event]) -> Result<()> {
    let Some(last) = events.last() else { return Ok(()) };
    let state = events.iter().fold(load_state(c)?, |s, e| domain.evolve(s, e));
    c.execute(
        "INSERT INTO state (id, json, at) VALUES (1, ?1, ?2) ON CONFLICT(id) DO UPDATE SET json = excluded.json, at = excluded.at",
        params![to_json!(&state)?, last.id.0 as i64],
    )
    .map_err(be)?;
    Ok(())
}

/// Writes one event the store causes itself (a lease's, a claim's, a text's), folded at once.
pub(crate) fn write_event(
    c: &Connection,
    domain: &dyn Domain,
    kind: EventKind,
    subject: Subject,
    payload: serde_json::Value,
    by: &Principal,
) -> Result<()> {
    let events = insert_drafts(c, vec![Draft { kind, subject, payload }], by, now())?;
    fold(c, domain, &events)
}

enum Outcome {
    Done(Applied),
    Crashed,
}

impl SqliteStore {
    fn run_apply(&self, conn: &mut Connection, cmd: Command, key: IdempotencyKey, by: Principal) -> Result<Outcome> {
        let domain = self.domain_or_err()?;
        let tx = conn.transaction_with_behavior(rusqlite::TransactionBehavior::Immediate).map_err(be)?;
        let first: Option<String> =
            tx.query_row("SELECT result FROM command WHERE key = ?1", [&key.0], |r| r.get(0)).optional().map_err(be)?;
        if let Some(first) = first {
            return Ok(Outcome::Done(from_json!(&first)?));
        }
        if let Some(fence) = fence_of(&cmd) {
            check_fence(&tx, fence)?;
        }
        let state = load_state(&tx)?;
        let drafts = domain.decide(&state, &cmd, &by).map_err(Error::Refused)?;
        if self.hook.compare_exchange(ARMED, DEAD, Ordering::SeqCst, Ordering::SeqCst).is_ok() {
            let half = drafts.len() / 2;
            insert_drafts(&tx, drafts.into_iter().take(half).collect(), &by, now())?;
            drop(tx); // uncommitted: rolled back, as a killed process's would be
            return Ok(Outcome::Crashed);
        }
        let events = insert_drafts(&tx, drafts, &by, now())?;
        fold(&tx, domain, &events)?;
        let applied = Applied { events: events.iter().map(|e| e.id).collect() };
        tx.execute("INSERT INTO command (key, result) VALUES (?1, ?2)", params![key.0, to_json!(&applied)?]).map_err(be)?;
        tx.commit().map_err(be)?;
        Ok(Outcome::Done(applied))
    }
}

impl Commands for SqliteStore {
    fn apply(&self, cmd: Command, key: IdempotencyKey, by: Principal) -> Result<Applied> {
        let mut g = self.lock();
        let conn = g.as_mut().ok_or_else(crashed)?;
        match self.run_apply(conn, cmd, key, by)? {
            Outcome::Done(a) => {
                drop(g);
                self.bump();
                Ok(a)
            }
            Outcome::Crashed => {
                *g = None; // dead: every later call is `Error::Backend`
                Err(Error::Backend { reason: "the handle crashed mid-apply (test hook)".into() })
            }
        }
    }
}
