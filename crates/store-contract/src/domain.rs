//! How `apply` validates without logic in the contract (docs/state-core.md
//! sections 2.2 and 3; wsp-043's review note, decision (a)).
//!
//! The contract declares the two signatures; a domain implements them. A
//! backend's `apply` runs [`Domain::decide`] on its current [`State`], stamps
//! the drafts it returns as events, and writes them in one atomic write; it
//! keeps its state as the fold of [`Domain::evolve`] over those events. The
//! conformance suite (wsp-047) brings a small test domain; `wsp-machines`
//! brings the real one (steps 2-4).

use crate::command::Command;
use crate::entity::State;
use crate::event::{Draft, Event};
use crate::id::Principal;
use crate::refusal::Refusal;

/// The machines of section 3, as two pure functions.
pub trait Domain {
    /// The events `cmd`, issued by `by`, would cause in `state`, or the named
    /// reason it is refused. Writes nothing; an empty vector is a command that
    /// changes nothing.
    fn decide(&self, state: &State, cmd: &Command, by: &Principal) -> Result<Vec<Draft>, Refusal>;

    /// `state` after `event`. Total: an event the domain does not know leaves
    /// the state as it was. It folds the events a store writes itself too
    /// (`lease.*`, `claim.*`, `text.set`; their payloads are in the `store`
    /// module's docs), since `snapshot()` is the fold of the whole log.
    fn evolve(&self, state: State, event: &Event) -> State;
}
