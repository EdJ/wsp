//! Refusals (docs/state-core.md sections 2.2, 3 and 4.3): every refusal is
//! returned with a named reason, never as a silent no-op. Tagged by `code`, as
//! 4.1's error reply carries it; a code this build does not know, from a newer
//! peer, reads as `Unknown`.

use serde::{Deserialize, Serialize};

use crate::event::Subject;
use crate::id::*;

/// Why the store or the domain said no.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "code", rename_all = "snake_case")]
pub enum Refusal {
    /// The command's fence names a token older than the lease's current one
    /// (2.2): the writer lost its lease and has not noticed yet.
    Stale { lease: LeaseName, current: Token },
    /// `events_after` asked from before what retention keeps (4.3, Q7): take a
    /// fresh snapshot.
    TooOld { oldest: EventId },
    /// `set_text` named a revision that is not the current one (2.2, 2.3 case 10).
    Conflict { current: Rev },
    /// The lease is held by someone else and has not expired.
    Held { lease: LeaseName, holder: Principal },
    /// Nothing has that id.
    NotFound { subject: Subject },
    /// The subject's state does not allow the verb (section 3's guards).
    Illegal {
        subject: Subject,
        state: String,
        verb: String,
    },
    /// The principal may not do this.
    Forbidden { by: Principal },
    /// The command is malformed, or a guard of its own failed; `reason` says which.
    Invalid { reason: String },
    /// A reason this build does not know, from a newer peer.
    #[serde(other)]
    Unknown,
}

/// What a store operation fails with: the domain's or the store's refusal, or
/// the backend could not do it at all (its file, its connection).
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Error {
    Refused(Refusal),
    Backend { reason: String },
}

/// A store operation's result.
pub type Result<T> = std::result::Result<T, Error>;
