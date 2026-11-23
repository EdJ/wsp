//! What the server serves: the hub's SQLite as a `dyn Store`, with one change
//! to its event log. The store writes a `text.set` for every refs write, which
//! says only that some workspace's refs changed; the hub's own announcement
//! row (see [`crate::Hub::move_ref`]) is read here as what it means, a
//! `ref.moved { workspace, subject, to }` (the multiplayer design's "a head
//! moved"), so a subscriber hears which subject to fetch. An expiry's
//! announcement row (see [`crate::leases`]) is read as `lease.expired
//! { workspace, row, holder, token }`, so the holder's spoke hears it lost
//! the lease.
//!
//! Event ids are the store's own and are kept: the events are rewritten in
//! place, one for one, so a cursor, a replay from `from` and the head all mean
//! what they mean in the store.

use std::collections::BTreeMap;
use std::sync::Arc;
use std::time::Duration;

use serde_json::json;
use wsp_api::{Refs, WorkspaceId as SyncWorkspace};
use wsp_store_contract::{
    Applied, Claim, Claims, Command, Commands, Event, EventId, EventKind, Events, Fence, IdempotencyKey, Lease,
    LeaseName, Leases, List, ListFilter, ListId, Machine, MachineName, Order, OrderState, Principal, Reading,
    Readings, Reads, Result, Rev, Row, RowFilter, RowId, Seat, SeatFilter, SeatId, State, Subject, Texts, Token,
};

use crate::leases::{lease_name, Expired, EXPIRED_PREFIX};
use crate::{Hub, MOVED_PREFIX};

/// The hub as a store: every call is the SQLite's, except that
/// [`Events::events_after`] reads an announcement row's `text.set` as a
/// `ref.moved`.
pub struct HubStore(Arc<Hub>);

impl HubStore {
    pub fn new(hub: Arc<Hub>) -> HubStore {
        HubStore(hub)
    }

    /// `event` as the stream shows it.
    fn announced(&self, event: Event) -> Event {
        let (EventKind::TextSet, Subject::Row(row)) = (event.kind, &event.subject) else {
            return event;
        };
        if row.0.starts_with(EXPIRED_PREFIX) {
            return expired(event);
        }
        let Some(workspace) = row.0.strip_prefix(MOVED_PREFIX) else {
            return event;
        };
        let moved = event
            .payload
            .get("section")
            .and_then(|s| s.as_str())
            .and_then(|s| serde_json::from_str::<Subject>(s).ok());
        let Some(moved) = moved else { return event };
        // The head as it is now: the one the subject was moved to, or a later one.
        let workspace_id = SyncWorkspace(workspace.to_string());
        let Ok(refs) = self.0.heads(&workspace_id) else { return event };
        let Some(to) = refs.get(&moved) else { return event };
        let payload = json!({ "workspace": workspace, "subject": moved, "to": to });
        Event {
            kind: EventKind::RefMoved,
            workspace: workspace.parse().unwrap_or_default(),
            subject: moved,
            payload,
            ..event
        }
    }

    /// `workspace`'s refs, for a test of the stream.
    pub fn heads(&self, workspace: &SyncWorkspace) -> Result<Refs> {
        self.0.heads(workspace)
    }
}

/// An expiry announcement's `text.set` as the `lease.expired` it means.
fn expired(event: Event) -> Event {
    let section = event.payload.get("section").and_then(|s| s.as_str());
    let Some(expired) = section.and_then(|s| serde_json::from_str::<Expired>(s).ok()) else { return event };
    Event {
        kind: EventKind::LeaseExpired,
        workspace: expired.workspace.0.parse().unwrap_or_default(),
        subject: Subject::Lease(lease_name(&expired.row)),
        payload: json!(expired),
        ..event
    }
}

impl Reads for HubStore {
    fn row(&self, id: &RowId) -> Result<Option<Row>> {
        self.0.db.row(id)
    }
    fn rows(&self, filter: &RowFilter) -> Result<Vec<Row>> {
        self.0.db.rows(filter)
    }
    fn seat(&self, id: &SeatId) -> Result<Option<Seat>> {
        self.0.db.seat(id)
    }
    fn seats(&self, filter: &SeatFilter) -> Result<Vec<Seat>> {
        self.0.db.seats(filter)
    }
    fn list(&self, id: &ListId) -> Result<Option<List>> {
        self.0.db.list(id)
    }
    fn orders_for(&self, seat: &SeatId, states: &[OrderState]) -> Result<Vec<Order>> {
        self.0.db.orders_for(seat, states)
    }
    fn claim(&self, row: &RowId) -> Result<Option<Claim>> {
        Reads::claim(&self.0.db, row)
    }
    fn lease(&self, name: &LeaseName) -> Result<Option<Lease>> {
        self.0.db.lease(name)
    }
    fn machine(&self, name: &MachineName) -> Result<Option<Machine>> {
        self.0.db.machine(name)
    }
    fn snapshot(&self) -> Result<(State, EventId)> {
        self.0.db.snapshot()
    }
    fn wip_snapshot(&self) -> Result<(State, EventId)> {
        self.0.db.wip_snapshot()
    }
    fn lists(&self, filter: &ListFilter) -> Result<Vec<List>> {
        self.0.db.lists(filter)
    }
}

impl Readings for HubStore {
    fn readings(&self, names: &[String]) -> Result<BTreeMap<String, Reading>> {
        self.0.db.readings(names)
    }
    fn set_reading(&self, name: &str, json: String) -> Result<()> {
        self.0.db.set_reading(name, json)
    }
}

impl Events for HubStore {
    fn events_after(&self, after: EventId, limit: usize) -> Result<Vec<Event>> {
        let events = self.0.db.events_after(after, limit)?;
        Ok(events.into_iter().map(|e| self.announced(e)).collect())
    }
    fn head(&self) -> Result<EventId> {
        self.0.db.head()
    }
    fn wait_for(&self, after: EventId, timeout: Duration) -> Result<EventId> {
        self.0.db.wait_for(after, timeout)
    }
}

impl Commands for HubStore {
    fn apply(&self, cmd: Command, key: IdempotencyKey, by: Principal) -> Result<Applied> {
        self.0.db.apply(cmd, key, by)
    }
}

impl Leases for HubStore {
    fn take(&self, name: &LeaseName, holder: &Principal, ttl: Duration) -> Result<Option<Token>> {
        Leases::take(&self.0.db, name, holder, ttl)
    }
    fn renew(&self, name: &LeaseName, token: Token, ttl: Duration) -> Result<bool> {
        self.0.db.renew(name, token, ttl)
    }
    fn release(&self, name: &LeaseName, token: Token) -> Result<()> {
        Leases::release(&self.0.db, name, token)
    }
}

impl Claims for HubStore {
    fn claim(&self, row: &RowId, seat: &SeatId, ttl: Duration, fence: Option<&Fence>) -> Result<Token> {
        Claims::claim(&self.0.db, row, seat, ttl, fence)
    }
    fn release(&self, row: &RowId, token: Token) -> Result<()> {
        Claims::release(&self.0.db, row, token)
    }
}

impl Texts for HubStore {
    fn text(&self, row: &RowId, section: &str) -> Result<(String, Rev)> {
        self.0.db.text(row, section)
    }
    fn set_text(&self, row: &RowId, section: &str, text: String, expect: Rev) -> Result<Rev> {
        self.0.db.set_text(row, section, text, expect)
    }
}
