//! The hub's leases on shared rows (wsp-466; the multiplayer design's "Leases
//! and offline sync"): `lease.take`, `lease.renew` and `lease.release`.
//!
//! A row's lease is wsp-store-sqlite's lease `row:<row>` (its `Leases`, so
//! the fencing token is the store's: one counter per row, kept across
//! releases, so every take after a release or an expiry is higher than any
//! before it). Around it the hub puts store-sqlite's claim shape
//! (crates/store-sqlite/src/leases.rs, `Claims::claim`): the holder taking it
//! again keeps its token and is renewed; another holder unexpired is refused
//! `held { holder }`; an expired one is released first and the take issues
//! the next token.
//!
//! **The clock.** Expiry is the hub's [`Clock`]: the wall clock, plus however
//! far a test has [`Clock::advance`]d it. The store stamps a lease's
//! `expires` on the wall clock, so the hub hands it the TTL plus that advance,
//! and a lease's `expires` reads on the hub's clock. The store would refuse to
//! take a lease it thinks unexpired; the hub releases one its own clock says
//! has expired before taking it, so the store never has to decide.
//!
//! **Expiry.** [`Hub::expire_leases`] (the server runs it every
//! [`SWEEP`], and a take or a renew of an expired lease does it for that one)
//! releases each lease past its `expires` and announces `lease.expired
//! { workspace, row, holder, token }`: the store's `text.set` of the
//! announcement row `hub-expired:<workspace>`, in a section that is the
//! payload's JSON, which [`crate::HubStore`] reads as the event. The holder's
//! spoke hears it and releases its claim as `wsp despawn --lost` does.
//!
//! **Allocation** (wsp-414). A row whose record (at its head in the
//! workspace) says `for: <user>` in its fields, or which is in a list that
//! does, is refused `forbidden` to any other user.
//!
//! The hub is one process; one lock orders every lease change in it, so a
//! take, a renew and the sweep never interleave.

use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Mutex;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};
use wsp_api::{LeaseHold, LeaseTake, WorkspaceId};
use wsp_objects::Object;
use wsp_store_contract::{
    At, Entity, Error, LeaseName, Leases, Principal, Reads, Refusal, Result, RowId, Subject, Texts, Token,
};

use crate::Hub;

/// A lease's TTL unless the config says otherwise (decided 2026-11-23).
pub const DEFAULT_TTL: Duration = Duration::from_secs(7 * 24 * 3600);

/// How often the server sweeps for expired leases.
pub const SWEEP: Duration = Duration::from_secs(60);

/// The row prefix of a workspace's expiry announcements.
pub(crate) const EXPIRED_PREFIX: &str = "hub-expired:";

/// The row prefix of the record of which workspace a row's lease was taken in.
const LEASED_PREFIX: &str = "hub-lease:";

/// The section of that record.
const WORKSPACE: &str = "workspace";

/// The field a record's allocation is in (wsp-414's `for:`).
const FOR: &str = "for";

/// The hub's clock: the wall clock, and how far a test has moved it on.
#[derive(Debug, Default)]
pub struct Clock {
    advanced_ms: AtomicU64,
}

impl Clock {
    /// Now, on this clock.
    pub fn now(&self) -> At {
        At(wall().0.saturating_add(self.advanced()))
    }

    /// Moves the clock on by `by` (a test's: expiry without waiting a week).
    pub fn advance(&self, by: Duration) {
        self.advanced_ms.fetch_add(by.as_millis() as u64, Ordering::SeqCst);
    }

    fn advanced(&self) -> u64 {
        self.advanced_ms.load(Ordering::SeqCst)
    }
}

fn wall() -> At {
    At(SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_millis() as u64).unwrap_or(0))
}

/// The lease state of the hub: its TTL, its clock, and the lock its changes take.
#[derive(Debug)]
pub(crate) struct LeaseDesk {
    pub(crate) ttl: Duration,
    pub(crate) clock: Clock,
    lock: Mutex<()>,
}

impl Default for LeaseDesk {
    fn default() -> LeaseDesk {
        LeaseDesk { ttl: DEFAULT_TTL, clock: Clock::default(), lock: Mutex::new(()) }
    }
}

/// A lease the hub holds for a row: what `lease.take` and `lease.renew` answer.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Held {
    pub token: Token,
    pub expires: At,
}

/// A lease that expired: what `lease.expired` announces.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Expired {
    pub workspace: WorkspaceId,
    pub row: RowId,
    pub holder: Principal,
    pub token: Token,
}

/// `row:<row>`, the hub's lease on a row.
pub fn lease_name(row: &RowId) -> LeaseName {
    LeaseName(format!("row:{}", row.0))
}

fn row_of(name: &LeaseName) -> Option<RowId> {
    name.0.strip_prefix("row:").map(|r| RowId(r.to_string()))
}

fn leased_row(row: &RowId) -> RowId {
    RowId(format!("{LEASED_PREFIX}{}", row.0))
}

fn refused(r: Refusal) -> Error {
    Error::Refused(r)
}

impl Hub {
    /// The hub's clock.
    pub fn clock(&self) -> &Clock {
        &self.desk.clock
    }

    /// A lease's TTL on this hub.
    pub fn lease_ttl(&self) -> Duration {
        self.desk.ttl
    }

    /// Sets the TTL of the leases taken or renewed from now (the config's `lease_days`).
    pub fn with_lease_ttl(mut self, ttl: Duration) -> Hub {
        self.desk.ttl = ttl;
        self
    }

    /// The TTL the store is handed, so that its wall-clock `expires` is the
    /// hub's clock's now plus the TTL.
    fn store_ttl(&self) -> Duration {
        self.desk.ttl + Duration::from_millis(self.desk.clock.advanced())
    }

    fn locked(&self) -> std::sync::MutexGuard<'_, ()> {
        self.desk.lock.lock().unwrap_or_else(|e| e.into_inner())
    }

    /// `row`'s lease as it stands, expired or not.
    pub fn row_lease(&self, row: &RowId) -> Result<Option<wsp_store_contract::Lease>> {
        self.db.lease(&lease_name(row))
    }

    /// `lease.take` for `by`, whose membership of `ask.workspace` the caller
    /// has checked: the allocation, then the claim shape (see the module).
    pub fn take_row_lease(&self, by: &Principal, ask: &LeaseTake) -> Result<Held> {
        if let (Principal::Person(user), Some(owner)) = (by, self.allocated_to(&ask.workspace, &ask.row)?) {
            if owner != *user {
                return Err(refused(Refusal::Forbidden { by: by.clone() }));
            }
        }
        let _guard = self.locked();
        let name = lease_name(&ask.row);
        if let Some(cur) = self.db.lease(&name)? {
            if cur.expires <= self.clock().now() {
                self.expire(&name, &cur)?;
            } else if cur.holder == *by {
                return self.renewed(&name, cur.token);
            } else {
                return Err(refused(Refusal::Held { lease: name, holder: cur.holder }));
            }
        }
        let Some(token) = Leases::take(&self.db, &name, by, self.store_ttl())? else {
            // No one holds it (it was released above, under the lock), so the store takes it.
            return Err(Error::Backend { reason: format!("the store would not take {}, which no one holds", name.0) });
        };
        self.record_workspace(&ask.row, &ask.workspace)?;
        let expires = self.db.lease(&name)?.map(|l| l.expires).unwrap_or_else(|| self.clock().now());
        Ok(Held { token, expires })
    }

    /// `lease.renew` for `by`.
    pub fn renew_row_lease(&self, by: &Principal, hold: &LeaseHold) -> Result<Held> {
        let _guard = self.locked();
        let name = lease_name(&hold.row);
        let Some(cur) = self.db.lease(&name)? else {
            return Err(refused(Refusal::NotFound { subject: Subject::Lease(name) }));
        };
        if cur.holder != *by {
            return Err(refused(Refusal::Held { lease: name, holder: cur.holder }));
        }
        if cur.token != hold.token {
            return Err(refused(Refusal::Stale { lease: name, current: cur.token }));
        }
        if cur.expires <= self.clock().now() {
            self.expire(&name, &cur)?;
            return Err(refused(Refusal::Stale { lease: name, current: cur.token }));
        }
        self.renewed(&name, cur.token)
    }

    /// `lease.release` for `by`: a token that is not the current one releases
    /// nothing and is answered as one that does.
    pub fn release_row_lease(&self, by: &Principal, hold: &LeaseHold) -> Result<()> {
        let _guard = self.locked();
        let name = lease_name(&hold.row);
        match self.db.lease(&name)? {
            Some(cur) if cur.token == hold.token && cur.holder != *by => {
                Err(refused(Refusal::Forbidden { by: by.clone() }))
            }
            Some(cur) if cur.token == hold.token => Leases::release(&self.db, &name, cur.token),
            _ => Ok(()),
        }
    }

    /// Releases every row lease past its `expires` on the hub's clock and
    /// announces each; answers what expired.
    pub fn expire_leases(&self) -> Result<Vec<Expired>> {
        let _guard = self.locked();
        let now = self.clock().now();
        let state = self.state()?;
        let mut out = Vec::new();
        for (name, lease) in &state.leases {
            if row_of(name).is_none() {
                continue;
            }
            // The fold may trail the table: read the lease itself.
            let Some(cur) = self.db.lease(name)? else { continue };
            if cur.token == lease.token && cur.expires <= now {
                if let Some(expired) = self.expire(name, &cur)? {
                    out.push(expired);
                }
            }
        }
        Ok(out)
    }

    /// Renews `name` for its holder of `token`, under the lock.
    fn renewed(&self, name: &LeaseName, token: Token) -> Result<Held> {
        if !Leases::renew(&self.db, name, token, self.store_ttl())? {
            return Err(refused(Refusal::Stale { lease: name.clone(), current: token }));
        }
        let expires = self.db.lease(name)?.map(|l| l.expires).unwrap_or_else(|| self.clock().now());
        Ok(Held { token, expires })
    }

    /// Releases the expired `cur` and announces it, under the lock. Answers
    /// the announcement, or none when the workspace it was taken in is not
    /// recorded.
    fn expire(&self, name: &LeaseName, cur: &wsp_store_contract::Lease) -> Result<Option<Expired>> {
        Leases::release(&self.db, name, cur.token)?;
        let Some(row) = row_of(name) else { return Ok(None) };
        let (workspace, _) = self.db.text(&leased_row(&row), WORKSPACE)?;
        if workspace.is_empty() {
            return Ok(None);
        }
        let expired = Expired { workspace: WorkspaceId(workspace), row, holder: cur.holder.clone(), token: cur.token };
        self.announce_expired(&expired);
        Ok(Some(expired))
    }

    /// Writes `lease.expired`'s announcement: like a move's (see
    /// [`Hub::move_ref`]), it follows the release and is no part of it.
    fn announce_expired(&self, expired: &Expired) {
        let Ok(section) = serde_json::to_string(expired) else { return };
        let row = RowId(format!("{EXPIRED_PREFIX}{}", expired.workspace.0));
        for _ in 0..64 {
            let Ok((_, rev)) = self.db.text(&row, &section) else { return };
            match self.db.set_text(&row, &section, String::new(), rev) {
                Err(Error::Refused(Refusal::Conflict { .. })) => continue,
                _ => return,
            }
        }
    }

    /// Records that `row`'s lease is `workspace`'s, for its expiry's announcement.
    fn record_workspace(&self, row: &RowId, workspace: &WorkspaceId) -> Result<()> {
        let at = leased_row(row);
        loop {
            let (body, rev) = self.db.text(&at, WORKSPACE)?;
            if body == workspace.0 {
                return Ok(());
            }
            match self.db.set_text(&at, WORKSPACE, workspace.0.clone(), rev) {
                Err(Error::Refused(Refusal::Conflict { .. })) => continue,
                other => return other.map(|_| ()),
            }
        }
    }

    /// The user `row` is allocated to in `workspace`: its record's `for`,
    /// else the `for` of a list in the workspace that holds it, else nobody.
    pub fn allocated_to(&self, workspace: &WorkspaceId, row: &RowId) -> Result<Option<String>> {
        let refs = self.heads(workspace)?;
        let record = |subject: &Subject| -> Result<Option<Entity>> {
            let Some(id) = refs.get(subject) else { return Ok(None) };
            let Some(bytes) = self.objects().get(id)? else { return Ok(None) };
            Ok(match wsp_objects::decode(&bytes) {
                Ok(Object::Record { entity, workspace: w, .. }) if w == *workspace => Some(entity),
                _ => None,
            })
        };
        let named = |fields: &std::collections::BTreeMap<String, String>| {
            fields.get(FOR).map(|u| u.trim().to_string()).filter(|u| !u.is_empty())
        };
        if let Some(Entity::Row(r)) = record(&Subject::Row(row.clone()))? {
            if let Some(user) = named(&r.fields) {
                return Ok(Some(user));
            }
        }
        for subject in refs.heads.keys().filter(|s| matches!(s, Subject::List(_))) {
            if let Some(Entity::List(list)) = record(subject)? {
                if list.groups.iter().any(|g| g.members.contains(row)) {
                    if let Some(user) = named(&list.fields) {
                        return Ok(Some(user));
                    }
                }
            }
        }
        Ok(None)
    }
}
