//! `MemStore`: the in-memory fake the suite first runs against, and a store
//! step 1's server tests may use. It implements the contract's whole trait
//! family (2.2) the way the contract's docs describe a backend:
//!
//! - **One lock.** Every handle on a store shares one `Arc`: the event log, the
//!   state (the fold of the domain's `evolve` over the log), the idempotency
//!   keys, the lease table, the texts, behind one mutex, and a condvar
//!   `wait_for` sleeps on. So `apply`'s fence check, `decide`, and its write are
//!   one atomic step, and so are a lease's check and its take.
//! - **`apply`** returns the first `Applied` for a key it has seen; refuses with
//!   `stale` when the command's [`fence_of`] is not its lease's current token
//!   (or the lease was released); runs `decide` on the state; stamps the drafts
//!   with the next ids, the clock and the principal; appends and folds them.
//!   A refused command writes nothing and records no key: a retry decides again.
//! - **Leases** keep the last token per name across releases, so `take` issues
//!   `last + 1` and tokens strictly increase. A claim is the lease `claim:<row>`
//!   held by `Principal::Seat`; claiming again by the holding seat extends it
//!   under the same token.
//! - **Events the store writes itself** (no command, so not from `decide`):
//!   `lease.taken` with the [`Lease`] as payload, on `take` and on `renew`;
//!   `lease.released` with subject `Lease(name)`; `claim.taken` with the
//!   [`Claim`], on `Claims::claim`; `claim.released` with subject `Claim(row)`;
//!   `text.set` with subject `Row(row)` and `{section, rev}`, by `wsp:store`.
//!   So `snapshot()` stays the fold of the log (case 6). The contract does not
//!   name these payloads yet (a gap; see the crate's MAP.md).
//! - **Texts** are kept beside the state, not in it (2.5): a section's revision
//!   counts its writes, `"0"` before the first.
//! - **No retention**: `events_after` never refuses with `too_old`.
//!
//! The test hook and the knobs:
//!
//! - [`MemStore::crash_next_apply`] arms one handle: its next `apply` decides,
//!   stages the first half of its events, and stops as a killed process would,
//!   returning `Error::Backend`; the staged half is discarded and the handle is
//!   dead (every later call fails). [`MemStore::reopen`] opens a new handle on
//!   what the store durably holds, refolding the state from the log (case 8).
//! - [`Knobs`] each break one guarantee, for the bite tests: all off by default,
//!   set with [`MemStore::set_knobs`] on any handle (they are the store's).

use std::collections::{BTreeMap, HashMap};
use std::sync::atomic::{AtomicU8, Ordering};
use std::sync::{Arc, Condvar, Mutex, MutexGuard};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use serde_json::{json, Value};
use wsp_store_contract::*;

use crate::domain::{claim_lease, TestDomain};

/// Test-only knobs: each breaks one guarantee of 2.3, so a test can show the
/// matching case fails (the cases must bite). All off by default.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Knobs {
    /// `apply` forgets every key: a repeated key applies again (case 2).
    pub ignore_idempotency: bool,
    /// `apply` and `Claims::claim` do not check a command's fence (case 4).
    pub skip_fence: bool,
    /// Every other commit is held back and published after the next one, so a
    /// lower id becomes visible after a higher one: a reader that saw the
    /// higher id skips it (case 5's hard case, 8.4).
    pub gap_in_ids: bool,
    /// The crash hook commits the half of the command it staged instead of
    /// discarding it (case 8).
    pub write_half: bool,
}

/// The fence a command is issued under, if any (2.2): what `apply` checks
/// before `decide`. The contract carries the fence in each fenced verb's
/// arguments and names no accessor, so a backend matches the verbs.
pub fn fence_of(cmd: &Command) -> Option<&Fence> {
    match cmd {
        Command::RowClaim { fence, .. } | Command::RowReview { fence, .. } | Command::RowLand { fence, .. } => {
            Some(fence)
        }
        Command::SeatEnd { fence, .. } => fence.as_ref(),
        _ => None,
    }
}

/// One lease name's slot: the last token issued, kept across releases, and
/// the lease while someone holds it.
#[derive(Clone, Debug, Default)]
struct Slot {
    last: Token,
    held: Option<Lease>,
}

struct Inner {
    /// Committed events, in the order they became visible (commit order,
    /// unless `gap_in_ids` reorders them).
    log: Vec<Event>,
    /// The last id issued.
    issued: u64,
    /// The greatest id visible.
    head: EventId,
    state: State,
    keys: HashMap<IdempotencyKey, Applied>,
    leases: BTreeMap<LeaseName, Slot>,
    texts: BTreeMap<(RowId, String), (String, u64)>,
    /// A commit `gap_in_ids` is holding back.
    late: Vec<Event>,
    knobs: Knobs,
}

struct Shared {
    inner: Mutex<Inner>,
    wake: Condvar,
    domain: Arc<dyn Domain + Send + Sync>,
}

const LIVE: u8 = 0;
const ARMED: u8 = 1;
const DEAD: u8 = 2;

/// A handle on an in-memory store. [`MemStore::handle`] opens another handle
/// on the same store, as a second process would; each handle has its own crash
/// hook.
pub struct MemStore {
    shared: Arc<Shared>,
    hook: AtomicU8,
}

impl Default for MemStore {
    fn default() -> Self {
        Self::new()
    }
}

impl std::fmt::Debug for MemStore {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("MemStore").field("hook", &self.hook.load(Ordering::SeqCst)).finish_non_exhaustive()
    }
}

fn now() -> At {
    At(SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_millis() as u64).unwrap_or(0))
}

fn plus(at: At, ttl: Duration) -> At {
    At(at.0.saturating_add(ttl.as_millis() as u64))
}

fn refused<T>(r: Refusal) -> Result<T> {
    Err(Error::Refused(r))
}

fn crashed<T>() -> Result<T> {
    Err(Error::Backend { reason: "the handle crashed mid-apply (test hook)".into() })
}

impl MemStore {
    /// An empty store with the test domain, and its first handle.
    pub fn new() -> Self {
        Self::with_domain(Arc::new(TestDomain))
    }

    /// An empty store whose `apply` validates through `domain`.
    pub fn with_domain(domain: Arc<dyn Domain + Send + Sync>) -> Self {
        let inner = Inner {
            log: vec![],
            issued: 0,
            head: EventId(0),
            state: State::default(),
            keys: HashMap::new(),
            leases: BTreeMap::new(),
            texts: BTreeMap::new(),
            late: vec![],
            knobs: Knobs::default(),
        };
        let shared = Arc::new(Shared { inner: Mutex::new(inner), wake: Condvar::new(), domain });
        MemStore { shared, hook: AtomicU8::new(LIVE) }
    }

    /// Another handle on this store.
    pub fn handle(&self) -> MemStore {
        MemStore { shared: Arc::clone(&self.shared), hook: AtomicU8::new(LIVE) }
    }

    /// Sets the store's knobs (every handle's).
    pub fn set_knobs(&self, knobs: Knobs) {
        self.lock().knobs = knobs;
    }

    pub fn knobs(&self) -> Knobs {
        self.lock().knobs
    }

    /// Arms the crash hook: the next `apply` through this handle stops midway
    /// and the handle is dead after it.
    pub fn crash_next_apply(&self) {
        let _ = self.hook.compare_exchange(LIVE, ARMED, Ordering::SeqCst, Ordering::SeqCst);
    }

    /// True once the crash hook has fired on this handle.
    pub fn is_dead(&self) -> bool {
        self.hook.load(Ordering::SeqCst) == DEAD
    }

    /// Reopens the store after a crash: a new handle on what it durably holds
    /// (the log, the keys, the lease table, the texts), with the state refolded
    /// from the log and any commit `gap_in_ids` held back lost, as an
    /// uncommitted transaction is.
    pub fn reopen(&self) -> MemStore {
        {
            let mut g = self.lock();
            let inner = &mut *g;
            inner.late.clear();
            let domain = &self.shared.domain;
            inner.state = inner.log.iter().fold(State::default(), |s, e| domain.evolve(s, e));
            inner.head = inner.log.iter().map(|e| e.id).max().unwrap_or_default();
            inner.issued = inner.head.0;
        }
        self.handle()
    }

    fn lock(&self) -> MutexGuard<'_, Inner> {
        // A panicking test thread must not wedge the others: the data is never
        // left half-written by a panic (no code between a stage and its commit panics).
        self.shared.inner.lock().unwrap_or_else(|e| e.into_inner())
    }

    fn live(&self) -> Result<()> {
        if self.is_dead() {
            crashed()
        } else {
            Ok(())
        }
    }

    /// Stamps `drafts` with the next ids, `at` and `by`.
    fn stamp(inner: &mut Inner, drafts: Vec<Draft>, by: &Principal) -> Vec<Event> {
        let at = now();
        drafts
            .into_iter()
            .map(|d| {
                inner.issued += 1;
                Event {
                    id: EventId(inner.issued),
                    at,
                    principal: by.clone(),
                    kind: d.kind,
                    subject: d.subject,
                    payload: d.payload,
                }
            })
            .collect()
    }

    /// Makes `events` visible: appends, folds, wakes. Under `gap_in_ids`,
    /// holds every other commit back until the next one is visible.
    fn publish(&self, inner: &mut Inner, events: Vec<Event>) {
        if events.is_empty() {
            return;
        }
        let mut out = events;
        if inner.knobs.gap_in_ids {
            if inner.late.is_empty() {
                inner.late = out;
                return;
            }
            out.append(&mut inner.late);
        }
        let domain = &self.shared.domain;
        for e in out {
            inner.head = inner.head.max(e.id);
            inner.state = domain.evolve(std::mem::take(&mut inner.state), &e);
            inner.log.push(e);
        }
        self.shared.wake.notify_all();
    }

    /// Writes one event the store causes itself (a lease's, a claim's, a text's).
    fn write(&self, inner: &mut Inner, kind: EventKind, subject: Subject, payload: Value, by: &Principal) {
        let events = Self::stamp(inner, vec![Draft { kind, subject, payload }], by);
        self.publish(inner, events);
    }

    /// `stale` unless `fence` names its lease's current token, held.
    fn check(inner: &Inner, fence: &Fence) -> Result<()> {
        if inner.knobs.skip_fence {
            return Ok(());
        }
        let slot = inner.leases.get(&fence.lease);
        let current = slot.map(|s| s.last).unwrap_or_default();
        match slot.and_then(|s| s.held.as_ref()) {
            Some(l) if l.token == fence.token => Ok(()),
            _ => refused(Refusal::Stale { lease: fence.lease.clone(), current }),
        }
    }
}

impl Reads for MemStore {
    fn row(&self, id: &RowId) -> Result<Option<Row>> {
        self.live()?;
        Ok(self.lock().state.rows.get(id).cloned())
    }

    fn rows(&self, filter: &RowFilter) -> Result<Vec<Row>> {
        self.live()?;
        let g = self.lock();
        Ok(g.state
            .rows
            .values()
            .filter(|r| filter.project.as_ref().is_none_or(|p| r.project == *p))
            .filter(|r| filter.states.is_empty() || filter.states.contains(&r.state))
            .filter(|r| filter.list.as_ref().is_none_or(|l| r.group.as_ref().is_some_and(|g| g.list == *l)))
            .filter(|r| filter.seat.as_ref().is_none_or(|s| r.seat.as_ref() == Some(s)))
            .cloned()
            .collect())
    }

    fn seat(&self, id: &SeatId) -> Result<Option<Seat>> {
        self.live()?;
        Ok(self.lock().state.seats.get(id).cloned())
    }

    fn seats(&self, filter: &SeatFilter) -> Result<Vec<Seat>> {
        self.live()?;
        let g = self.lock();
        Ok(g.state
            .seats
            .values()
            .filter(|s| filter.machine.as_ref().is_none_or(|m| s.machine == *m))
            .filter(|s| filter.states.is_empty() || filter.states.contains(&s.state))
            .filter(|s| filter.row.as_ref().is_none_or(|r| s.row.as_ref() == Some(r)))
            .cloned()
            .collect())
    }

    fn list(&self, id: &ListId) -> Result<Option<List>> {
        self.live()?;
        Ok(self.lock().state.lists.get(id).cloned())
    }

    fn orders_for(&self, seat: &SeatId, states: &[OrderState]) -> Result<Vec<Order>> {
        self.live()?;
        let g = self.lock();
        Ok(g.state
            .orders
            .values()
            .filter(|o| o.to == *seat && (states.is_empty() || states.contains(&o.state)))
            .cloned()
            .collect())
    }

    fn claim(&self, row: &RowId) -> Result<Option<Claim>> {
        self.live()?;
        Ok(self.lock().state.claims.get(row).cloned())
    }

    /// The lease while someone holds it (expired or not), claims' `claim:<row>` included.
    fn lease(&self, name: &LeaseName) -> Result<Option<Lease>> {
        self.live()?;
        Ok(self.lock().leases.get(name).and_then(|s| s.held.clone()))
    }

    fn machine(&self, name: &MachineName) -> Result<Option<Machine>> {
        self.live()?;
        Ok(self.lock().state.machines.get(name).cloned())
    }

    fn snapshot(&self) -> Result<(State, EventId)> {
        self.live()?;
        let g = self.lock();
        Ok((g.state.clone(), g.head))
    }
}

impl Events for MemStore {
    fn events_after(&self, after: EventId, limit: usize) -> Result<Vec<Event>> {
        self.live()?;
        let g = self.lock();
        Ok(g.log.iter().filter(|e| e.id > after).take(limit).cloned().collect())
    }

    fn head(&self) -> Result<EventId> {
        self.live()?;
        Ok(self.lock().head)
    }

    fn wait_for(&self, after: EventId, timeout: Duration) -> Result<EventId> {
        self.live()?;
        let deadline = Instant::now() + timeout;
        let mut g = self.lock();
        while g.head <= after {
            let left = deadline.saturating_duration_since(Instant::now());
            if left.is_zero() {
                break;
            }
            g = self.shared.wake.wait_timeout(g, left).unwrap_or_else(|e| e.into_inner()).0;
        }
        Ok(g.head)
    }
}

impl Commands for MemStore {
    fn apply(&self, cmd: Command, key: IdempotencyKey, by: Principal) -> Result<Applied> {
        self.live()?;
        let mut g = self.lock();
        let inner = &mut *g;
        if !inner.knobs.ignore_idempotency {
            if let Some(first) = inner.keys.get(&key) {
                return Ok(first.clone());
            }
        }
        if let Some(fence) = fence_of(&cmd) {
            Self::check(inner, fence)?;
        }
        let drafts = self.shared.domain.decide(&inner.state, &cmd, &by).map_err(Error::Refused)?;
        if self.hook.compare_exchange(ARMED, DEAD, Ordering::SeqCst, Ordering::SeqCst).is_ok() {
            let half = drafts.len() / 2;
            let staged = Self::stamp(inner, drafts.into_iter().take(half).collect(), &by);
            if inner.knobs.write_half {
                self.publish(inner, staged);
            } else {
                // The ids staged were never visible; give them back, as a rolled-back transaction does.
                inner.issued -= staged.len() as u64;
            }
            return crashed();
        }
        let events = Self::stamp(inner, drafts, &by);
        let applied = Applied { events: events.iter().map(|e| e.id).collect() };
        self.publish(inner, events);
        inner.keys.insert(key, applied.clone());
        Ok(applied)
    }
}

impl Leases for MemStore {
    fn take(&self, name: &LeaseName, holder: &Principal, ttl: Duration) -> Result<Option<Token>> {
        self.live()?;
        let mut g = self.lock();
        let inner = &mut *g;
        let at = now();
        let slot = inner.leases.entry(name.clone()).or_default();
        if slot.held.as_ref().is_some_and(|l| l.expires > at) {
            return Ok(None);
        }
        slot.last = Token(slot.last.0 + 1);
        let lease = Lease { name: name.clone(), holder: holder.clone(), token: slot.last, expires: plus(at, ttl) };
        slot.held = Some(lease.clone());
        let token = lease.token;
        self.write(inner, EventKind::LeaseTaken, Subject::Lease(name.clone()), json!(lease), holder);
        Ok(Some(token))
    }

    fn renew(&self, name: &LeaseName, token: Token, ttl: Duration) -> Result<bool> {
        self.live()?;
        let mut g = self.lock();
        let inner = &mut *g;
        let Some(lease) = inner.leases.get_mut(name).and_then(|s| s.held.as_mut()).filter(|l| l.token == token)
        else {
            return Ok(false);
        };
        lease.expires = plus(now(), ttl);
        let lease = lease.clone();
        self.write(inner, EventKind::LeaseTaken, Subject::Lease(name.clone()), json!(lease), &lease.holder);
        Ok(true)
    }

    fn release(&self, name: &LeaseName, token: Token) -> Result<()> {
        self.live()?;
        let mut g = self.lock();
        let inner = &mut *g;
        let Some(slot) = inner.leases.get_mut(name).filter(|s| s.held.as_ref().is_some_and(|l| l.token == token))
        else {
            return Ok(());
        };
        let holder = slot.held.take().map(|l| l.holder).expect("held, as filtered");
        self.write(inner, EventKind::LeaseReleased, Subject::Lease(name.clone()), Value::Null, &holder);
        Ok(())
    }
}

impl Claims for MemStore {
    fn claim(&self, row: &RowId, seat: &SeatId, ttl: Duration, fence: Option<&Fence>) -> Result<Token> {
        self.live()?;
        let mut g = self.lock();
        let inner = &mut *g;
        if let Some(fence) = fence {
            Self::check(inner, fence)?;
        }
        let name = claim_lease(row);
        let who = Principal::Seat(seat.clone());
        let at = now();
        let slot = inner.leases.entry(name.clone()).or_default();
        let token = match &slot.held {
            Some(l) if l.holder == who => l.token,
            Some(l) if l.expires > at => return refused(Refusal::Held { lease: name, holder: l.holder.clone() }),
            _ => {
                slot.last = Token(slot.last.0 + 1);
                slot.last
            }
        };
        let expires = plus(at, ttl);
        slot.held = Some(Lease { name, holder: who.clone(), token, expires });
        let claim = Claim { row: row.clone(), seat: seat.clone(), token, expires };
        self.write(inner, EventKind::ClaimTaken, Subject::Claim(row.clone()), json!(claim), &who);
        Ok(token)
    }

    fn release(&self, row: &RowId, token: Token) -> Result<()> {
        self.live()?;
        let mut g = self.lock();
        let inner = &mut *g;
        let name = claim_lease(row);
        let Some(slot) = inner.leases.get_mut(&name).filter(|s| s.held.as_ref().is_some_and(|l| l.token == token))
        else {
            return Ok(());
        };
        let holder = slot.held.take().map(|l| l.holder).expect("held, as filtered");
        self.write(inner, EventKind::ClaimReleased, Subject::Claim(row.clone()), Value::Null, &holder);
        Ok(())
    }
}

impl Texts for MemStore {
    fn text(&self, row: &RowId, section: &str) -> Result<(String, Rev)> {
        self.live()?;
        let g = self.lock();
        let (text, rev) = g.texts.get(&(row.clone(), section.to_string())).cloned().unwrap_or_default();
        Ok((text, Rev(rev.to_string())))
    }

    fn set_text(&self, row: &RowId, section: &str, text: String, expect: Rev) -> Result<Rev> {
        self.live()?;
        let mut g = self.lock();
        let inner = &mut *g;
        let entry = inner.texts.entry((row.clone(), section.to_string())).or_default();
        let current = Rev(entry.1.to_string());
        if expect != current {
            return refused(Refusal::Conflict { current });
        }
        *entry = (text, entry.1 + 1);
        let rev = Rev(entry.1.to_string());
        let by = Principal::Wsp("store".into());
        self.write(inner, EventKind::TextSet, Subject::Row(row.clone()), json!({ "section": section, "rev": rev }), &by);
        Ok(rev)
    }
}
