//! The store's trait family (docs/state-core.md section 2.2): domain verbs,
//! with no SQL, connection, transaction handle or backend row type (D1). A
//! backend chooses how to make each one atomic; the conformance suite (2.3,
//! wsp-047) is what says it did.
//!
//! Every trait is object-safe: `&self` methods, no generic methods, no `Self`
//! returned, so `&dyn Store` works. Methods that share a name across traits
//! (`claim`, `release`) are called through their trait (`Claims::release(&s, ..)`).

use std::time::Duration;

use crate::command::{Applied, Command, Fence};
use crate::entity::*;
use crate::event::Event;
use crate::id::*;
use crate::refusal::Result;

/// Which rows `rows` returns; an empty field does not filter.
#[derive(Clone, Debug, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct RowFilter {
    #[serde(default)]
    pub project: Option<ProjectId>,
    #[serde(default)]
    pub states: Vec<RowState>,
    #[serde(default)]
    pub list: Option<ListId>,
    #[serde(default)]
    pub seat: Option<SeatId>,
}

/// Which seats `seats` returns; an empty field does not filter.
#[derive(Clone, Debug, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct SeatFilter {
    #[serde(default)]
    pub machine: Option<MachineName>,
    #[serde(default)]
    pub states: Vec<SeatState>,
    #[serde(default)]
    pub row: Option<RowId>,
}

/// Reads. `None` is "no such thing", not an error.
pub trait Reads {
    fn row(&self, id: &RowId) -> Result<Option<Row>>;
    fn rows(&self, filter: &RowFilter) -> Result<Vec<Row>>;
    fn seat(&self, id: &SeatId) -> Result<Option<Seat>>;
    fn seats(&self, filter: &SeatFilter) -> Result<Vec<Seat>>;
    fn list(&self, id: &ListId) -> Result<Option<List>>;
    /// A seat's orders in any of `states`; all of them when `states` is empty.
    fn orders_for(&self, seat: &SeatId, states: &[OrderState]) -> Result<Vec<Order>>;
    fn claim(&self, row: &RowId) -> Result<Option<Claim>>;
    fn lease(&self, name: &LeaseName) -> Result<Option<Lease>>;
    fn machine(&self, name: &MachineName) -> Result<Option<Machine>>;
    /// The whole domain state and the id it is consistent at.
    fn snapshot(&self) -> Result<(State, EventId)>;
}

/// The event log.
pub trait Events {
    /// Up to `limit` events with ids greater than `after`, in commit order, with
    /// no gap a reader can skip (2.3 case 5). Refused with `TooOld` when `after`
    /// is before what retention keeps.
    fn events_after(&self, after: EventId, limit: usize) -> Result<Vec<Event>>;
    /// The id of the last committed event; `EventId(0)` when there is none.
    fn head(&self) -> Result<EventId>;
    /// Returns once the head is past `after`, or `timeout` has passed, with the
    /// head as it then is.
    fn wait_for(&self, after: EventId, timeout: Duration) -> Result<EventId>;
}

/// Commands.
pub trait Commands {
    /// Validate `cmd` by its machine, and write its state change and its events
    /// atomically. A second `apply` with the same `key` returns the first result
    /// and writes nothing.
    fn apply(&self, cmd: Command, key: IdempotencyKey, by: Principal) -> Result<Applied>;
}

/// Leases (2.2, 8.2).
pub trait Leases {
    /// Takes `name` for `holder` if it is free or expired, issuing the last
    /// token plus one; `None` if someone else holds it.
    fn take(&self, name: &LeaseName, holder: &Principal, ttl: Duration) -> Result<Option<Token>>;
    /// Extends the lease if `token` is still current; `false` if it is not.
    fn renew(&self, name: &LeaseName, token: Token, ttl: Duration) -> Result<bool>;
    /// Gives the lease up if `token` is still current.
    fn release(&self, name: &LeaseName, token: Token) -> Result<()>;
}

/// Claims: the lease `claim:<row>`, held by a seat (2.2).
pub trait Claims {
    fn claim(
        &self,
        row: &RowId,
        seat: &SeatId,
        ttl: Duration,
        fence: Option<&Fence>,
    ) -> Result<Token>;
    fn release(&self, row: &RowId, token: Token) -> Result<()>;
}

/// The prose that stays in files (2.2, 2.5).
pub trait Texts {
    fn text(&self, row: &RowId, section: &str) -> Result<(String, Rev)>;
    /// Writes the section if `expect` is its current revision, returning the
    /// new one; refused with `Conflict` otherwise.
    fn set_text(&self, row: &RowId, section: &str, text: String, expect: Rev) -> Result<Rev>;
}

/// The whole family: what the conformance suite is generic over.
pub trait Store: Reads + Events + Commands + Leases + Claims + Texts {}

impl<T: Reads + Events + Commands + Leases + Claims + Texts> Store for T {}
