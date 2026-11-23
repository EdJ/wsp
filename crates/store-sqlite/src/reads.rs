//! The reads, from the state row and the event table; `Texts` and `wait_for` wait for later legs.

use std::time::Duration;

use rusqlite::{params, OptionalExtension};
use wsp_store_contract::*;

use crate::apply::{from_json, head_of, write_event};
use crate::{be, SqliteStore};

impl Reads for SqliteStore {
    fn row(&self, id: &RowId) -> Result<Option<Row>> {
        Ok(self.snapshot()?.0.rows.get(id).cloned())
    }

    fn rows(&self, filter: &RowFilter) -> Result<Vec<Row>> {
        let all = match filter.heads {
            // Cut in SQL, as `wip_snapshot` is, so the text is never decoded (wsp-743).
            true => self.cut("$.rows", true)?.iter().map(|t| from_json!(t)).collect::<Result<Vec<Row>>>()?,
            false => self.snapshot()?.0.rows.into_values().collect(),
        };
        Ok(all
            .iter()
            .filter(|r| filter.project.as_ref().is_none_or(|p| r.project == *p))
            .filter(|r| filter.states.is_empty() || filter.states.contains(&r.state))
            .filter(|r| filter.list.as_ref().is_none_or(|l| r.group.as_ref().is_some_and(|g| g.list == *l)))
            .filter(|r| filter.seat.as_ref().is_none_or(|s| r.seat.as_ref() == Some(s)))
            .cloned()
            .collect())
    }

    fn seat(&self, id: &SeatId) -> Result<Option<Seat>> {
        Ok(self.snapshot()?.0.seats.get(id).cloned())
    }

    fn seats(&self, filter: &SeatFilter) -> Result<Vec<Seat>> {
        let (state, _) = self.snapshot()?;
        Ok(state
            .seats
            .values()
            .filter(|s| filter.machine.as_ref().is_none_or(|m| s.machine == *m))
            .filter(|s| filter.states.is_empty() || filter.states.contains(&s.state))
            .filter(|s| filter.row.as_ref().is_none_or(|r| s.row.as_ref() == Some(r)))
            .cloned()
            .collect())
    }

    fn list(&self, id: &ListId) -> Result<Option<List>> {
        Ok(self.snapshot()?.0.lists.get(id).cloned())
    }

    /// Over the state row's lists alone, whole or cut in SQL: the rows are never decoded.
    fn lists(&self, filter: &ListFilter) -> Result<Vec<List>> {
        self.cut("$.lists", filter.heads)?.iter().map(|t| from_json!(t)).collect()
    }

    fn orders_for(&self, seat: &SeatId, states: &[OrderState]) -> Result<Vec<Order>> {
        let (state, _) = self.snapshot()?;
        Ok(state
            .orders
            .values()
            .filter(|o| o.to == *seat && (states.is_empty() || states.contains(&o.state)))
            .cloned()
            .collect())
    }

    fn claim(&self, row: &RowId) -> Result<Option<Claim>> {
        Ok(self.snapshot()?.0.claims.get(row).cloned())
    }

    /// The lease while someone holds it (expired or not), claims' `claim:<row>` included.
    fn lease(&self, name: &LeaseName) -> Result<Option<Lease>> {
        self.with_conn(|c| crate::leases::held(c, name))
    }

    fn machine(&self, name: &MachineName) -> Result<Option<Machine>> {
        Ok(self.snapshot()?.0.machines.get(name).cloned())
    }

    fn snapshot(&self) -> Result<(State, EventId)> {
        self.with_conn(|c| {
            // One statement, so the state row and the head are of one moment.
            let (json, head): (Option<String>, i64) = c
                .query_row(
                    "SELECT (SELECT json FROM state WHERE id = 1), (SELECT COALESCE(MAX(id), 0) FROM event)",
                    [],
                    |r| Ok((r.get(0)?, r.get(1)?)),
                )
                .map_err(be)?;
            let state = match json {
                Some(j) => from_json!(&j)?,
                None => State::default(),
            };
            Ok((state, EventId(head as u64)))
        })
    }

    /// Cut down in SQL, over the state row, so the full domain is never decoded into Rust: one
    /// statement, so the cut and the head are of one moment, as `snapshot()`'s are.
    fn wip_snapshot(&self) -> Result<(State, EventId)> {
        let word = |v: serde_json::Value| v.as_str().unwrap_or_default().to_string();
        let done = word(serde_json::json!(RowState::Done));
        let raised = word(serde_json::json!(AskState::Raised));
        let finished = word(serde_json::json!(ListState::Finished));
        let [body, prose] = WIP_BODIES.map(|k| format!("$.fields.{k}"));
        let seated = format!("$.fields.{WIP_SEATED}");
        self.with_conn(|c| {
            let (json, head): (String, i64) = c
                .query_row(
                    "SELECT json_object(
                        'rows', (SELECT json_group_object(key,
                                    CASE WHEN json_extract(value, '$.state') = ?1
                                         THEN json_remove(value, ?3, ?4) ELSE json(value) END)
                                 FROM json_each(s.json, '$.rows')),
                        'lists', (SELECT json_group_object(key,
                                     CASE WHEN json_extract(value, '$.state') = ?5
                                               AND json_extract(value, ?6) IS NULL
                                          THEN json_remove(value, ?3, ?4) ELSE json(value) END)
                                  FROM json_each(s.json, '$.lists')),
                        'seats', json(COALESCE(json_extract(s.json, '$.seats'), '{}')),
                        'claims', json(COALESCE(json_extract(s.json, '$.claims'), '{}')),
                        'asks', (SELECT json_group_object(key, json(value))
                                 FROM json_each(s.json, '$.asks')
                                 WHERE json_extract(value, '$.state') = ?2)),
                     (SELECT COALESCE(MAX(id), 0) FROM event)
                     FROM (SELECT (SELECT json FROM state WHERE id = 1) AS json) AS s",
                    params![done, raised, body, prose, finished, seated],
                    |r| Ok((r.get(0)?, r.get(1)?)),
                )
                .map_err(be)?;
            Ok((from_json!(&json)?, EventId(head as u64)))
        })
    }
}

impl SqliteStore {
    /// The JSON of each entity under `path` (`$.rows`, `$.lists`) of the state row, in key
    /// order (a `BTreeMap`'s), each without its `WIP_BODIES` fields where `heads`.
    fn cut(&self, path: &str, heads: bool) -> Result<Vec<String>> {
        let [body, prose] = WIP_BODIES.map(|k| format!("$.fields.{k}"));
        self.with_conn(|c| {
            let mut stmt = c
                .prepare_cached(
                    "SELECT CASE WHEN ?2 THEN json_remove(e.value, ?3, ?4) ELSE json(e.value) END
                     FROM state AS s, json_each(s.json, ?1) AS e WHERE s.id = 1 ORDER BY e.key",
                )
                .map_err(be)?;
            let texts = stmt.query_map(params![path, heads, body, prose], |r| r.get::<_, String>(0)).map_err(be)?;
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
                    "SELECT id, at, principal, kind, subject, payload FROM event WHERE id > ?1 ORDER BY id LIMIT ?2",
                )
                .map_err(be)?;
            let limit = i64::try_from(limit).unwrap_or(i64::MAX);
            let rows = stmt
                .query_map(params![after.0 as i64, limit], |r| {
                    Ok((r.get::<_, i64>(0)?, r.get::<_, i64>(1)?, r.get::<_, String>(2)?, r.get::<_, String>(3)?, r.get::<_, String>(4)?, r.get::<_, String>(5)?))
                })
                .map_err(be)?;
            let mut out = vec![];
            for row in rows {
                let (id, at, principal, kind, subject, payload) = row.map_err(be)?;
                out.push(Event {
                    id: EventId(id as u64),
                    at: At(at as u64),
                    principal: from_json!(&principal)?,
                    kind: from_json!(&kind)?,
                    subject: from_json!(&subject)?,
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
            write_event(tx, domain, EventKind::TextSet, Subject::Row(row.clone()), serde_json::json!({ "section": section, "rev": next }), &by)?;
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
