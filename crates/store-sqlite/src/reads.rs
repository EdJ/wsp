//! The reads, from the entity rows (or the handle's cache of them) and the event table; `Texts` and `wait_for` wait for later legs.

use std::time::Duration;

use rusqlite::{params, OptionalExtension};
use wsp_store_contract::*;

use crate::apply::{from_json, head_of, write_event};
use crate::entities::{key_text, one, with_state};
use crate::{be, SqliteStore};

impl Reads for SqliteStore {
    fn row(&self, id: &RowId) -> Result<Option<Row>> {
        self.entity("rows", &key_text!(id), |j| from_json!(j))
    }

    fn rows(&self, filter: &RowFilter) -> Result<Vec<Row>> {
        let keep = |r: &Row| {
            filter.project.as_ref().is_none_or(|p| r.project == *p)
                && (filter.states.is_empty() || filter.states.contains(&r.state))
                && filter.list.as_ref().is_none_or(|l| r.group.as_ref().is_some_and(|g| g.list == *l))
                && filter.seat.as_ref().is_none_or(|s| r.seat.as_ref() == Some(s))
        };
        match filter.heads {
            // Cut in SQL, as `wip_snapshot` is, so the bodies are never decoded (wsp-743).
            true => Ok(self.cut("rows", true)?.iter().map(|t| from_json!(t)).collect::<Result<Vec<Row>>>()?.into_iter().filter(keep).collect()),
            false => self.read(|c| with_state(c, &self.cache, |s| s.rows.values().filter(|r| keep(r)).cloned().collect())),
        }
    }

    fn seat(&self, id: &SeatId) -> Result<Option<Seat>> {
        self.entity("seats", &key_text!(id), |j| from_json!(j))
    }

    fn seats(&self, filter: &SeatFilter) -> Result<Vec<Seat>> {
        self.read(|c| {
            with_state(c, &self.cache, |state| {
                state
                    .seats
                    .values()
                    .filter(|s| filter.machine.as_ref().is_none_or(|m| s.machine == *m))
                    .filter(|s| filter.states.is_empty() || filter.states.contains(&s.state))
                    .filter(|s| filter.row.as_ref().is_none_or(|r| s.row.as_ref() == Some(r)))
                    .cloned()
                    .collect()
            })
        })
    }

    fn list(&self, id: &ListId) -> Result<Option<List>> {
        self.entity("lists", &key_text!(id), |j| from_json!(j))
    }

    /// Over the lists' rows alone, whole or cut in SQL: the rows are never decoded.
    fn lists(&self, filter: &ListFilter) -> Result<Vec<List>> {
        self.cut("lists", filter.heads)?.iter().map(|t| from_json!(t)).collect()
    }

    fn orders_for(&self, seat: &SeatId, states: &[OrderState]) -> Result<Vec<Order>> {
        self.read(|c| {
            with_state(c, &self.cache, |state| {
                state
                    .orders
                    .values()
                    .filter(|o| o.to == *seat && (states.is_empty() || states.contains(&o.state)))
                    .cloned()
                    .collect()
            })
        })
    }

    fn claim(&self, row: &RowId) -> Result<Option<Claim>> {
        self.entity("claims", &key_text!(row), |j| from_json!(j))
    }

    /// The lease while someone holds it (expired or not), claims' `claim:<row>` included.
    fn lease(&self, name: &LeaseName) -> Result<Option<Lease>> {
        self.with_conn(|c| crate::leases::held(c, name))
    }

    fn machine(&self, name: &MachineName) -> Result<Option<Machine>> {
        self.entity("machines", &key_text!(name), |j| from_json!(j))
    }

    fn snapshot(&self) -> Result<(State, EventId)> {
        // One read transaction, so the state and the head are of one moment.
        self.read(|c| {
            let state = with_state(c, &self.cache, State::clone)?;
            Ok((state, EventId(head_of(c)?)))
        })
    }

    /// Cut down in SQL, over the entity rows, so the full domain is never decoded into Rust: one
    /// statement, so the cut and the head are of one moment, as `snapshot()`'s are. Seats that
    /// are gone, ending or lost, or that the census reads as gone, are left out (wsp-373).
    fn wip_snapshot(&self) -> Result<(State, EventId)> {
        let word = |v: serde_json::Value| v.as_str().unwrap_or_default().to_string();
        let done = word(serde_json::json!(RowState::Done));
        let raised = word(serde_json::json!(AskState::Raised));
        let finished = word(serde_json::json!(ListState::Finished));
        let [body, prose] = WIP_BODIES.map(|k| format!("$.fields.{k}"));
        let seated = format!("$.fields.{WIP_SEATED}");
        let census = format!("$.fields.{WIP_CENSUS}");
        self.with_conn(|c| {
            let (json, head): (String, i64) = c
                .query_row(
                    "SELECT json_object(
                        'rows', (SELECT json_group_object(key,
                                    CASE WHEN json_extract(json, '$.state') = ?1
                                         THEN json_remove(json, ?3, ?4) ELSE json(json) END)
                                 FROM entity WHERE map = 'rows'),
                        'lists', (SELECT json_group_object(key,
                                     CASE WHEN json_extract(json, '$.state') = ?5
                                               AND json_extract(json, ?6) IS NULL
                                          THEN json_remove(json, ?3, ?4) ELSE json(json) END)
                                  FROM entity WHERE map = 'lists'),
                        'seats', (SELECT json_group_object(key, json(json)) FROM entity
                                  WHERE map = 'seats'
                                    AND json_extract(json, '$.state') NOT IN ('gone', 'ending', 'lost')
                                    AND COALESCE(json_extract(json, ?7), '') <> 'gone'),
                        'claims', (SELECT json_group_object(key, json(json)) FROM entity WHERE map = 'claims'),
                        'asks', (SELECT json_group_object(key, json(json)) FROM entity
                                 WHERE map = 'asks' AND json_extract(json, '$.state') = ?2)),
                     (SELECT COALESCE(MAX(id), 0) FROM event)",
                    params![done, raised, body, prose, finished, seated, census],
                    |r| Ok((r.get(0)?, r.get(1)?)),
                )
                .map_err(be)?;
            Ok((from_json!(&json)?, EventId(head as u64)))
        })
    }
}

impl SqliteStore {
    /// Runs `f` in one read transaction: what it reads is of one moment.
    fn read<T>(&self, f: impl FnOnce(&rusqlite::Connection) -> Result<T>) -> Result<T> {
        self.with_conn(|c| {
            let tx = c.unchecked_transaction().map_err(be)?;
            let out = f(&tx);
            drop(tx); // it wrote nothing: the rollback only ends it
            out
        })
    }

    /// One entity of `map`, decoded alone (by `decode`: this crate names no `serde` bound).
    fn entity<T>(&self, map: &str, key: &str, decode: impl FnOnce(&str) -> Result<T>) -> Result<Option<T>> {
        self.with_conn(|c| one(c, map, key, decode))
    }

    /// The JSON of each entity of `map` (`rows`, `lists`), in key order (a `BTreeMap`'s), each
    /// without its `WIP_BODIES` fields where `heads`.
    fn cut(&self, map: &str, heads: bool) -> Result<Vec<String>> {
        let [body, prose] = WIP_BODIES.map(|k| format!("$.fields.{k}"));
        self.with_conn(|c| {
            let mut stmt = c
                .prepare_cached(
                    "SELECT CASE WHEN ?2 THEN json_remove(json, ?3, ?4) ELSE json(json) END
                     FROM entity WHERE map = ?1 ORDER BY key",
                )
                .map_err(be)?;
            let texts = stmt.query_map(params![map, heads, body, prose], |r| r.get::<_, String>(0)).map_err(be)?;
            texts.map(|t| t.map_err(be)).collect()
        })
    }
}

impl Readings for SqliteStore {
    /// From the `reading` table, beside the state row: no event, no fold.
    fn readings(&self, names: &[String]) -> Result<std::collections::BTreeMap<String, Reading>> {
        self.with_conn(|c| {
            let mut stmt = c.prepare_cached("SELECT name, json, at FROM reading ORDER BY name").map_err(be)?;
            let rows = stmt
                .query_map([], |r| Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?, r.get::<_, i64>(2)?)))
                .map_err(be)?;
            let mut out = std::collections::BTreeMap::new();
            for row in rows {
                let (name, json, at) = row.map_err(be)?;
                if names.is_empty() || names.contains(&name) {
                    out.insert(name, Reading { json, at: At(at as u64) });
                }
            }
            Ok(out)
        })
    }

    fn set_reading(&self, name: &str, json: String) -> Result<()> {
        self.with_conn(|c| {
            c.execute(
                "INSERT INTO reading (name, json, at) VALUES (?1, ?2, ?3)
                 ON CONFLICT(name) DO UPDATE SET json = excluded.json, at = excluded.at",
                params![name, json, crate::now().0 as i64],
            )
            .map_err(be)?;
            Ok(())
        })
    }
}

impl Events for SqliteStore {
    fn events_after(&self, after: EventId, limit: usize) -> Result<Vec<Event>> {
        self.with_conn(|c| {
            let mut stmt = c
                .prepare_cached(
                    "SELECT id, at, principal, kind, subject, payload, workspace FROM event WHERE id > ?1 ORDER BY id LIMIT ?2",
                )
                .map_err(be)?;
            let limit = i64::try_from(limit).unwrap_or(i64::MAX);
            let rows = stmt
                .query_map(params![after.0 as i64, limit], |r| {
                    Ok((r.get::<_, i64>(0)?, r.get::<_, i64>(1)?, r.get::<_, String>(2)?, r.get::<_, String>(3)?, r.get::<_, String>(4)?, r.get::<_, String>(5)?, r.get::<_, String>(6)?))
                })
                .map_err(be)?;
            let mut out = vec![];
            for row in rows {
                let (id, at, principal, kind, subject, payload, workspace) = row.map_err(be)?;
                out.push(Event {
                    id: EventId(id as u64),
                    at: At(at as u64),
                    principal: from_json!(&principal)?,
                    kind: from_json!(&kind)?,
                    subject: from_json!(&subject)?,
                    workspace: workspace.parse().map_err(be)?,
                    payload: from_json!(&payload)?,
                });
            }
            Ok(out)
        })
    }

    fn head(&self) -> Result<EventId> {
        self.with_conn(|c| Ok(EventId(head_of(c)?)))
    }

    fn wait_for(&self, after: EventId, timeout: Duration) -> Result<EventId> {
        self.wait_events(after, timeout)
    }
}

impl Texts for SqliteStore {
    fn text(&self, row: &RowId, section: &str) -> Result<(String, Rev)> {
        self.with_conn(|c| {
            let (body, rev) = text_of(c, row, section)?;
            Ok((body, Rev(rev.to_string())))
        })
    }

    /// Kept beside the state, not in it (2.5): a section's revision counts its writes, `"0"` before the first.
    fn set_text(&self, row: &RowId, section: &str, text: String, expect: Rev) -> Result<Rev> {
        let domain = self.domain_or_err()?;
        self.tx(|tx| {
            let (_, rev) = text_of(tx, row, section)?;
            let current = Rev(rev.to_string());
            if expect != current {
                return Err(Error::Refused(Refusal::Conflict { current }));
            }
            let next = Rev((rev + 1).to_string());
            tx.execute(
                "INSERT INTO text (row, section, body, rev) VALUES (?1, ?2, ?3, ?4)
                 ON CONFLICT(row, section) DO UPDATE SET body = excluded.body, rev = excluded.rev",
                params![row.0, section, text, rev + 1],
            )
            .map_err(be)?;
            let by = Principal::Wsp(STORE_INSTANCE.into());
            write_event(tx, &self.cache, domain, EventKind::TextSet, Subject::Row(row.clone()), serde_json::json!({ "section": section, "rev": next }), &by)?;
            Ok(next)
        })
    }
}

fn text_of(c: &rusqlite::Connection, row: &RowId, section: &str) -> Result<(String, i64)> {
    let r: Option<(String, i64)> = c
        .query_row("SELECT body, rev FROM text WHERE row = ?1 AND section = ?2", params![row.0, section], |r| {
            Ok((r.get(0)?, r.get(1)?))
        })
        .optional()
        .map_err(be)?;
    Ok(r.unwrap_or_default())
}
