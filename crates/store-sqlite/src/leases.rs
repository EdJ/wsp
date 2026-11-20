//! `Leases` and `Claims`: the lease table, each change with its event (the table in
//! store-contract's `store` module) in the same transaction, folded into the state row.
//! A lease's `last` token is kept across releases, so tokens strictly increase.

use std::time::Duration;

use rusqlite::{params, Connection, OptionalExtension};
use serde_json::{json, Value};
use wsp_store_contract::*;

use crate::apply::{check_fence, from_json, to_json, write_event};
use crate::{be, now, plus, SqliteStore};

/// `claim:<row>`, the lease a claim is (2.2).
fn claim_lease(row: &RowId) -> LeaseName {
    LeaseName(format!("claim:{}", row.0))
}

/// One lease name's slot: the last token issued, and the lease while someone holds it.
fn slot(c: &Connection, name: &LeaseName) -> Result<(Token, Option<Lease>)> {
    let r: Option<(i64, Option<String>, Option<i64>, Option<i64>)> = c
        .query_row("SELECT last, holder, token, expires FROM lease WHERE name = ?1", [&name.0], |r| {
            Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?))
        })
        .optional()
        .map_err(be)?;
    let Some((last, holder, token, expires)) = r else { return Ok((Token(0), None)) };
    let held = match (holder, token, expires) {
        (Some(h), Some(t), Some(e)) => {
            Some(Lease { name: name.clone(), holder: from_json!(&h)?, token: Token(t as u64), expires: At(e as u64) })
        }
        _ => None,
    };
    Ok((Token(last as u64), held))
}

/// The lease while someone holds it (expired or not).
pub(crate) fn held(c: &Connection, name: &LeaseName) -> Result<Option<Lease>> {
    Ok(slot(c, name)?.1)
}

fn put(c: &Connection, name: &LeaseName, last: Token, held: Option<&Lease>) -> Result<()> {
    let (holder, token, expires) = match held {
        Some(l) => (Some(to_json!(&l.holder)?), Some(l.token.0 as i64), Some(l.expires.0 as i64)),
        None => (None, None, None),
    };
    c.execute(
        "INSERT INTO lease (name, last, holder, token, expires) VALUES (?1, ?2, ?3, ?4, ?5)
         ON CONFLICT(name) DO UPDATE SET last = excluded.last, holder = excluded.holder,
             token = excluded.token, expires = excluded.expires",
        params![name.0, last.0 as i64, holder, token, expires],
    )
    .map_err(be)?;
    Ok(())
}

impl Leases for SqliteStore {
    fn take(&self, name: &LeaseName, holder: &Principal, ttl: Duration) -> Result<Option<Token>> {
        let domain = self.domain_or_err()?;
        self.tx(|tx| {
            let at = now();
            let (last, cur) = slot(tx, name)?;
            if cur.is_some_and(|l| l.expires > at) {
                return Ok(None);
            }
            let last = Token(last.0 + 1);
            let lease = Lease { name: name.clone(), holder: holder.clone(), token: last, expires: plus(at, ttl) };
            put(tx, name, last, Some(&lease))?;
            write_event(tx, domain, EventKind::LeaseTaken, Subject::Lease(name.clone()), json!(lease), holder)?;
            Ok(Some(last))
        })
    }

    fn renew(&self, name: &LeaseName, token: Token, ttl: Duration) -> Result<bool> {
        let domain = self.domain_or_err()?;
        self.tx(|tx| {
            let (last, cur) = slot(tx, name)?;
            let Some(mut lease) = cur.filter(|l| l.token == token) else { return Ok(false) };
            lease.expires = plus(now(), ttl);
            put(tx, name, last, Some(&lease))?;
            write_event(tx, domain, EventKind::LeaseTaken, Subject::Lease(name.clone()), json!(lease), &lease.holder)?;
            Ok(true)
        })
    }

    fn release(&self, name: &LeaseName, token: Token) -> Result<()> {
        let domain = self.domain_or_err()?;
        self.tx(|tx| {
            let (last, cur) = slot(tx, name)?;
            let Some(lease) = cur.filter(|l| l.token == token) else { return Ok(()) };
            put(tx, name, last, None)?;
            write_event(tx, domain, EventKind::LeaseReleased, Subject::Lease(name.clone()), Value::Null, &lease.holder)
        })
    }
}

impl Claims for SqliteStore {
    fn claim(&self, row: &RowId, seat: &SeatId, ttl: Duration, fence: Option<&Fence>) -> Result<Token> {
        let domain = self.domain_or_err()?;
        self.tx(|tx| {
            if let Some(fence) = fence {
                check_fence(tx, fence)?;
            }
            let name = claim_lease(row);
            let who = Principal::Seat(seat.clone());
            let at = now();
            let (last, cur) = slot(tx, &name)?;
            let (token, last) = match &cur {
                Some(l) if l.holder == who => (l.token, last),
                Some(l) if l.expires > at => {
                    return Err(Error::Refused(Refusal::Held { lease: name, holder: l.holder.clone() }))
                }
                _ => (Token(last.0 + 1), Token(last.0 + 1)),
            };
            let expires = plus(at, ttl);
            put(tx, &name, last, Some(&Lease { name: name.clone(), holder: who.clone(), token, expires }))?;
            let claim = Claim { row: row.clone(), seat: seat.clone(), token, expires };
            write_event(tx, domain, EventKind::ClaimTaken, Subject::Claim(row.clone()), json!(claim), &who)?;
            Ok(token)
        })
    }

    fn release(&self, row: &RowId, token: Token) -> Result<()> {
        let domain = self.domain_or_err()?;
        self.tx(|tx| {
            let name = claim_lease(row);
            let (last, cur) = slot(tx, &name)?;
            let Some(lease) = cur.filter(|l| l.token == token) else { return Ok(()) };
            put(tx, &name, last, None)?;
            write_event(tx, domain, EventKind::ClaimReleased, Subject::Claim(row.clone()), Value::Null, &lease.holder)
        })
    }
}
