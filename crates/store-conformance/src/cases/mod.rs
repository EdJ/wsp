//! The ten cases of 2.3, each a `pub fn case_x<H: Harness>(h: &H) -> CaseResult`,
//! so a backend's tests call them one by one with no exceptions file
//! (`case_fencing(&harness).unwrap()`). Concurrency cases race threads behind a
//! [`Barrier`](std::sync::Barrier) and bound their waits in seconds; none
//! sleeps hoping for an interleaving.
//!
//! **How a case fails.** It returns `Err(`[`CaseFailure`]`)`, never panics on a
//! broken guarantee: the failure names the case (its number and name in 2.3)
//! and says what was broken, with the values seen. A store error where the case
//! expected an answer is a failure too, so is a racing thread's panic. The bite
//! tests match on `case` (and may read `broken`); a backend's tests `unwrap()`,
//! which prints the failure.
//!
//! The cases, one file each: 1 `case_atomicity`, 2 `case_idempotency`,
//! 3 `case_lease_exclusivity`, 4 `case_fencing`, 5 `case_event_order`,
//! 6 `case_snapshot`, 7 `case_wakeups`, 8 `case_crash`, 9 `case_machine_guards`,
//! 10 `case_text`. Each [`Knobs`](crate::mem::Knobs) field makes its case fail
//! (`tests/mem.rs`, the bite tests).

use std::fmt;

use wsp_store_contract::{
    Applied, Claims, Command, EventId, Fence, IdempotencyKey, Principal, ProjectId, RowId, SeatId, Store, Subject,
};

use crate::domain::{claim_lease, verb};

mod atomicity;
mod crash;
mod fencing;
mod guards;
mod idempotency;
mod leases;
mod order;
mod snapshot;
mod text;
mod wakeups;

pub use atomicity::case_atomicity;
pub use crash::case_crash;
pub use fencing::case_fencing;
pub use guards::case_machine_guards;
pub use idempotency::case_idempotency;
pub use leases::case_lease_exclusivity;
pub use order::case_event_order;
pub use snapshot::case_snapshot;
pub use text::case_text;
pub use wakeups::case_wakeups;

/// A guarantee of 2.3 the backend broke: which case, and what it saw.
#[derive(Clone, PartialEq, Eq)]
pub struct CaseFailure {
    /// The case's number in 2.3 (1-10).
    pub case: u8,
    /// The case's name in 2.3 (`idempotency`).
    pub name: &'static str,
    /// What was broken, with the values seen.
    pub broken: String,
}

impl fmt::Display for CaseFailure {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "2.3 case {} ({}) broken: {}", self.case, self.name, self.broken)
    }
}

/// As [`Display`](fmt::Display), so `unwrap()` prints a readable failure.
impl fmt::Debug for CaseFailure {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        fmt::Display::fmt(self, f)
    }
}

impl std::error::Error for CaseFailure {}

/// What every case returns.
pub type CaseResult = Result<(), CaseFailure>;

/// A running case: the name its failures carry.
#[derive(Clone, Copy)]
pub(crate) struct Case {
    pub case: u8,
    pub name: &'static str,
}

impl Case {
    pub fn fail(&self, broken: impl Into<String>) -> CaseFailure {
        CaseFailure { case: self.case, name: self.name, broken: broken.into() }
    }

    /// A failure saying `broken` unless `ok`.
    pub fn ensure(&self, ok: bool, broken: impl FnOnce() -> String) -> CaseResult {
        if ok {
            Ok(())
        } else {
            Err(self.fail(broken()))
        }
    }

    /// A store answer the case needs: an error is a failure, saying what the case was doing.
    pub fn store<T>(&self, r: wsp_store_contract::Result<T>, doing: impl fmt::Display) -> Result<T, CaseFailure> {
        r.map_err(|e| self.fail(format!("{doing}: the store failed with {e:?}")))
    }

    /// A racing thread's result: its panic is a failure.
    pub fn joined<T>(&self, r: std::thread::Result<Result<T, CaseFailure>>) -> Result<T, CaseFailure> {
        r.unwrap_or_else(|p| {
            let text = p
                .downcast_ref::<&str>()
                .map(|s| s.to_string())
                .or_else(|| p.downcast_ref::<String>().cloned())
                .unwrap_or_else(|| "a non-text panic".into());
            Err(self.fail(format!("a racing thread panicked: {text}")))
        })
    }

    /// Applies a setup command, which the domain allows: a refusal is a failure.
    pub fn apply<S: Store>(&self, s: &S, cmd: Command, k: impl fmt::Display) -> Result<Applied, CaseFailure> {
        let doing = format!("setting up, {} (key {k})", verb(&cmd));
        self.store(s.apply(cmd, key(k), person()), doing)
    }

    /// The subject of the event `id`, read back from the log.
    pub fn subject_of<S: Store>(&self, s: &S, id: EventId) -> Result<Subject, CaseFailure> {
        let events = self.store(s.events_after(EventId(id.0.saturating_sub(1)), 1), "reading an event back")?;
        events
            .into_iter()
            .next()
            .filter(|e| e.id == id)
            .map(|e| e.subject)
            .ok_or_else(|| self.fail(format!("event {} was applied, but events_after does not return it", id.0)))
    }

    /// The subject of the first event `cmd` writes.
    fn first_subject<S: Store>(&self, s: &S, cmd: Command, k: impl fmt::Display) -> Result<Subject, CaseFailure> {
        let applied = self.apply(s, cmd, k)?;
        let first = *applied.events.first().ok_or_else(|| self.fail("setting up: a command wrote no event"))?;
        self.subject_of(s, first)
    }

    /// Adds a row (`row.add`) and returns its id.
    pub fn add_row<S: Store>(&self, s: &S, k: impl fmt::Display) -> Result<RowId, CaseFailure> {
        let cmd = Command::RowAdd {
            project: ProjectId("conformance".into()),
            title: k.to_string(),
            fields: Default::default(),
        };
        match self.first_subject(s, cmd, k)? {
            Subject::Row(id) => Ok(id),
            other => Err(self.fail(format!("row.add's event names {other:?}, not a row"))),
        }
    }

    /// Asks for a seat (`seat.spawn`, no row) and returns its id.
    pub fn spawn_seat<S: Store>(&self, s: &S, k: impl fmt::Display) -> Result<SeatId, CaseFailure> {
        let cmd = Command::SeatSpawn { row: None, attempt: 0, machine: None, kind: "claude".into(), model: None };
        match self.first_subject(s, cmd, k)? {
            Subject::Seat(id) => Ok(id),
            other => Err(self.fail(format!("seat.spawn's event names {other:?}, not a seat"))),
        }
    }

    /// Takes `row`'s claim for `seat` and returns its fence, without `row.claim`.
    pub fn take_claim<S: Store>(&self, s: &S, row: &RowId, seat: &SeatId) -> Result<Fence, CaseFailure> {
        let doing = format!("setting up, claiming {} for {}", row.0, seat.0);
        let token = self.store(Claims::claim(s, row, seat, LONG, None), doing)?;
        Ok(Fence { lease: claim_lease(row), token })
    }

    /// Claims `row` for `seat` and moves it to `doing` (`row.claim`); returns the claim's fence.
    pub fn claim_row<S: Store>(
        &self,
        s: &S,
        row: &RowId,
        seat: &SeatId,
        k: impl fmt::Display,
    ) -> Result<Fence, CaseFailure> {
        let fence = self.take_claim(s, row, seat)?;
        self.apply(s, Command::RowClaim { row: row.clone(), seat: seat.clone(), fence: fence.clone() }, k)?;
        Ok(fence)
    }
}

/// A lease or claim no case outlives.
pub(crate) const LONG: std::time::Duration = std::time::Duration::from_secs(600);

/// Who the cases' commands are by.
pub(crate) fn person() -> Principal {
    Principal::Person("conformance".into())
}

pub(crate) fn key(k: impl fmt::Display) -> IdempotencyKey {
    IdempotencyKey(k.to_string())
}
