//! The store's trait family (docs/state-core.md section 2.2): domain verbs,
//! with no SQL, connection, transaction handle or backend row type (D1). A
//! backend chooses how to make each one atomic; the conformance suite (2.3,
//! wsp-047) is what says it did.
//!
//! Every trait is object-safe: `&self` methods, no generic methods, no `Self`
//! returned, so `&dyn Store` works. Methods that share a name across traits
//! (`claim`, `release`) are called through their trait (`Claims::release(&s, ..)`).
//!
//! **Events a store writes itself** (wsp-065). `Leases`, `Claims` and `Texts`
//! change what `snapshot()` returns without a command, so no `decide` drafts
//! their events; the backend writes them, in the same atomic step as the
//! change, so `snapshot()` stays the fold of [`Domain::evolve`](crate::Domain::evolve)
//! over the log (2.3 case 6). A domain's `evolve` folds them as this table says,
//! and every backend writes exactly these:
//!
//! | call | kind | subject | payload | principal |
//! |---|---|---|---|---|
//! | `Leases::take` | `lease.taken` | `Lease(name)` | the [`Lease`] | the holder |
//! | `Leases::renew` | `lease.taken` | `Lease(name)` | the [`Lease`], same token, new `expires` | the holder |
//! | `Leases::release` | `lease.released` | `Lease(name)` | `null` | the holder |
//! | `Claims::claim` | `claim.taken` | `Claim(row)` | the [`Claim`] | `Seat(seat)` |
//! | `Claims::release` | `claim.released` | `Claim(row)` | `null` | the holder |
//! | `Texts::set_text` | `text.set` | `Row(row)` | `{"section", "rev"}` | `Wsp(`[`STORE_INSTANCE`]`)` |
//! | `Commands::apply` of `mirror`, each `put` | `mirror.put` | the entity's | the [`Entity`] | the command's |
//! | `Commands::apply` of `mirror`, each `gone` | `mirror.gone` | the subject | `null` | the command's |
//! | `Commands::apply` of `mirror`, a beat-only machine `put` | `machine.beat` | `Machine(name)` | the `Machine` | the command's |
//!
//! A renewal is a `lease.taken` again (there is no `lease.renewed`): the fold
//! keeps `expires` current by replacing the lease, so `evolve` reads `taken`
//! as "the lease is now this". A claim's lease (`claim:<row>`) writes the
//! `claim.*` kinds only, never `lease.*`. A call that changes nothing (a
//! `renew` or `release` under a stale token, a refused take or claim) writes
//! no event.
//!
//! The three `mirror` rows (wsp-070, step 1 alone) are not the backend's: the
//! server's mirror domain (wsp-073) drafts them from `decide`, and the backend
//! stamps them as any command's. They are in the table so that every `evolve`
//! folds them alike: `mirror.put` replaces the entity at its subject (a
//! `Claim`'s subject is `Claim(row)`), and `mirror.gone` removes what the
//! subject names. `machine.beat` (wsp-304) is a `put` of a machine whose beat
//! and liveness facts alone moved, and is folded as a `put` is; it has its own
//! kind so that a subscriber can tell a heartbeat from a change. A real domain
//! refuses `mirror` as `illegal`.

use std::time::Duration;

use crate::command::{Applied, Command, Fence};
use crate::entity::*;
use crate::event::Event;
use crate::id::*;
use crate::refusal::Result;

/// The instance a store's own writes are by, where its verb takes no principal:
/// `Texts::set_text` writes `text.set` by `Principal::Wsp(STORE_INSTANCE)`.
pub const STORE_INSTANCE: &str = "store";

/// The `fields` keys that hold a row's or a list's text, which
/// [`Reads::wip_snapshot`] leaves out of every row that is not open and every
/// list that is `finished`: `body` is the file's text, `prose` a head-only
/// row's body (as `src/mirror.rs` writes them). A list's `body` is the only
/// way back to its worklist, so one that is not finished keeps it, and so
/// does a finished one carrying [`WIP_SEATED`].
pub const WIP_BODIES: [&str; 2] = ["body", "prose"];

/// The list `fields` key `src/mirror.rs` writes (`"true"`) on a finished list
/// that still has a seat on it: a check whose entry names a pane and has not
/// ended, or a governor's post that is still filled. `wip` reads such a list
/// (its check seat's list and group, its governor's `of: list`), so
/// [`Reads::wip_snapshot`] keeps its [`WIP_BODIES`] (wsp-695).
pub const WIP_SEATED: &str = "seated";

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
    /// Each row as its head, without the [`WIP_BODIES`] fields (wsp-743): what
    /// `ls` draws, under one line where the rows whole are not. Not a filter:
    /// it cuts what each row carries, never which rows. Left out of the wire
    /// when false, so a filter without it is the frame it always was.
    #[serde(default, skip_serializing_if = "is_false")]
    pub heads: bool,
}

/// What `lists` returns (wsp-743): every list, each whole unless `heads`.
/// Read from `{}`, `null` or no `args` at all, as `NoArgs` was before it.
#[derive(Clone, Debug, Default, PartialEq, Eq, serde::Serialize)]
pub struct ListFilter {
    /// Each list without its [`WIP_BODIES`] fields, its groups whole. Left
    /// out of the wire when false.
    #[serde(default, skip_serializing_if = "is_false")]
    pub heads: bool,
}

impl<'de> serde::Deserialize<'de> for ListFilter {
    fn deserialize<D: serde::Deserializer<'de>>(d: D) -> std::result::Result<Self, D::Error> {
        #[derive(serde::Deserialize)]
        struct Given {
            #[serde(default)]
            heads: bool,
        }
        // Through `Option`, so that `null` and `args` left out read as the default.
        Ok(Option::<Given>::deserialize(d)?.map(|g| ListFilter { heads: g.heads }).unwrap_or_default())
    }
}

fn is_false(b: &bool) -> bool {
    !*b
}

/// One reading (wsp-743): a value the daemon works out from what it reads and
/// no entity carries (the throttle's readings, what `spend` sums, `wip`'s
/// whole census), as JSON text, and when it was written. Its shape is the
/// reader's to know by its name (`wsp-view`'s `reading` module); the store
/// keeps the text and the time and nothing else.
#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct Reading {
    /// The JSON text, as written.
    pub json: String,
    /// When it was written, by the store's clock.
    pub at: At,
}

/// Removes the [`WIP_BODIES`] fields: a row or a list as its head.
pub fn heads_only(fields: &mut std::collections::BTreeMap<String, String>) {
    WIP_BODIES.iter().for_each(|k| drop(fields.remove(*k)));
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
    /// What `wip` draws of [`snapshot`](Reads::snapshot), at the id it is
    /// consistent at (wsp-695): every row, an open one (any state but `done`)
    /// whole and any other as its head, which is the row without the
    /// [`WIP_BODIES`] fields; every list, one not `finished` whole, a finished
    /// one carrying [`WIP_SEATED`] whole too, and any other finished one
    /// without those fields, its groups whole; only the asks
    /// that are `raised`; and the seats and claims whole. Orders, runs, machines and leases
    /// are left empty. It is `snapshot()` with exactly that taken out, nothing
    /// else changed.
    fn wip_snapshot(&self) -> Result<(State, EventId)>;
    /// Every list in id order, each whole, or without its [`WIP_BODIES`]
    /// fields where `filter.heads` (wsp-743); its groups whole either way.
    /// The default cuts [`snapshot`](Reads::snapshot); a backend whose state
    /// is large cuts it where it is kept.
    fn lists(&self, filter: &ListFilter) -> Result<Vec<List>> {
        let (state, _) = self.snapshot()?;
        Ok(state
            .lists
            .into_values()
            .map(|mut l| {
                if filter.heads {
                    heads_only(&mut l.fields);
                }
                l
            })
            .collect())
    }
}

/// Readings (wsp-743): named values kept beside the state, not in it. A write
/// replaces the value and its time, and writes no event: a reading is a
/// reader's convenience that its writer takes again on a period, not a change
/// to the domain, so `snapshot()` and the log never hold one, and subscribers
/// are not woken by one. A backend that keeps none answers every read empty
/// and refuses a write as `Backend`.
pub trait Readings {
    /// The readings named, or every one when `names` is empty; a name with no
    /// reading is absent from the map.
    fn readings(&self, names: &[String]) -> Result<std::collections::BTreeMap<String, Reading>> {
        let _ = names;
        Ok(Default::default())
    }
    /// Replaces `name`'s reading with `json`, stamped now.
    fn set_reading(&self, name: &str, json: String) -> Result<()> {
        let _ = (name, json);
        Err(crate::Error::Backend { reason: "this store keeps no readings".into() })
    }
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
    /// atomically: first the fence (refused with `stale` when `cmd` is one of
    /// [`FENCED_VERBS`](crate::FENCED_VERBS) and its fence is not its lease's
    /// current token), then [`Domain::decide`](crate::Domain::decide), then the
    /// write.
    ///
    /// Idempotency (wsp-065): a second `apply` with the same `key` returns the
    /// first `Applied` and writes nothing. The key is the identity: a key reused
    /// with a different command still returns the first `Applied`, and the
    /// second command is not decided. A refusal is no result: it records no key,
    /// so a retry with the same key is decided again, on the state as it then
    /// is. Only an `Applied` (an empty one included) is kept.
    fn apply(&self, cmd: Command, key: IdempotencyKey, by: Principal) -> Result<Applied>;
}

/// Leases (2.2, 8.2). Each change writes its event (module docs): `take` and
/// `renew` a `lease.taken` carrying the [`Lease`], `release` a
/// `lease.released`, all by the holder.
pub trait Leases {
    /// Takes `name` for `holder` if it is free or expired, issuing the last
    /// token plus one; `None` if someone else holds it.
    fn take(&self, name: &LeaseName, holder: &Principal, ttl: Duration) -> Result<Option<Token>>;
    /// Extends the lease if `token` is still current, writing `lease.taken`
    /// again with the same token and the new `expires`; `false`, and no event,
    /// if it is not.
    fn renew(&self, name: &LeaseName, token: Token, ttl: Duration) -> Result<bool>;
    /// Gives the lease up if `token` is still current (`lease.released`, payload
    /// `null`); nothing, and no event, if it is not.
    fn release(&self, name: &LeaseName, token: Token) -> Result<()>;
}

/// Claims: the lease `claim:<row>`, held by a seat (2.2). They write the
/// `claim.*` kinds, never `lease.*` (module docs).
pub trait Claims {
    /// Takes or extends `claim:<row>` for `seat`, writing `claim.taken` with the
    /// [`Claim`] by `Seat(seat)`; the holding seat claiming again extends it
    /// under the same token. Refused with `held` while another seat holds it,
    /// and with `stale` when `fence` is given and not current.
    fn claim(
        &self,
        row: &RowId,
        seat: &SeatId,
        ttl: Duration,
        fence: Option<&Fence>,
    ) -> Result<Token>;
    /// Gives the claim up if `token` is still current (`claim.released`, payload
    /// `null`, by the holder); nothing, and no event, if it is not.
    fn release(&self, row: &RowId, token: Token) -> Result<()>;
}

/// The prose that stays in files (2.2, 2.5).
pub trait Texts {
    fn text(&self, row: &RowId, section: &str) -> Result<(String, Rev)>;
    /// Writes the section if `expect` is its current revision, returning the
    /// new one; refused with `Conflict` otherwise. It takes no principal, so its
    /// `text.set` (subject `Row(row)`, payload `{"section", "rev"}`) is by
    /// `Principal::Wsp(`[`STORE_INSTANCE`]`)`; the text itself stays out of the
    /// event and out of `State` (2.5).
    fn set_text(&self, row: &RowId, section: &str, text: String, expect: Rev) -> Result<Rev>;
}

/// The whole family: what the conformance suite is generic over.
pub trait Store: Reads + Events + Commands + Leases + Claims + Texts + Readings {}

impl<T: Reads + Events + Commands + Leases + Claims + Texts + Readings> Store for T {}
