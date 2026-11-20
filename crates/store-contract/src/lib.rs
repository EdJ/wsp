//! `wsp-store-contract`: what the other components may name of the records
//! without depending on the store (`docs/components.md` R2, cut `c5`). Types,
//! traits and constants only; no logic and no IO. See `MAP.md`.
//!
//! The constants below are today's file store's: the record schema, the section
//! names of a task, a project and a worklist, a machine's default lease, and the
//! group policy words. Its types (`Task`, `Project`, `Worklist`, `Machine`)
//! carry inherent methods, which Rust keeps in the crate that defines the type,
//! so they stay in `wsp-store`.
//!
//! The modules are the state core's contract (docs/state-core.md sections 2.1,
//! 2.2, 3, 4.2 and 4.3; wsp-046): the ids, the entities and their states, the
//! events, the commands, the refusals, subscribe's frames, the `Domain` that
//! validates a command, and the `Store` trait family a backend implements.
//! Nothing in wsp uses them yet. Every change to this crate is a change to the
//! contract (R5).

pub mod command;
pub mod domain;
pub mod entity;
pub mod event;
pub mod id;
pub mod refusal;
pub mod store;
pub mod subscribe;

pub use command::{Applied, Block, Command, Fence, ReleaseTo, Verdict, FENCED_VERBS};
pub use domain::Domain;
pub use entity::*;
pub use event::{Draft, Event, EventKind, Subject};
pub use id::*;
pub use refusal::{Error, Refusal, Result};
pub use store::{
    Claims, Commands, Events, Leases, Reads, RowFilter, SeatFilter, Store, Texts, STORE_INSTANCE,
    WIP_BODIES,
};
pub use subscribe::{Feed, Subscribe, BEAT_SECS, MISSED_BEATS};

pub const SCHEMA: &str = "1";

/// The sections a person writes on a **task**.
///
/// The list exists so that "which sections can be edited" is answered in one
/// place. It used to be answered in three, by three literals, and the third —
/// `edit_prose`'s — was still naming two of them after `Decisions` shipped.
/// `wsp edit --decisions` therefore fell through to the combined-buffer path
/// and overwrote `Overview` while reporting success. A vocabulary that lives in
/// one place cannot go stale in two others.
///
/// `Log` is not here. It is append-only, `wsp note` is how you add to it, and
/// editing history in place is how history stops being evidence.
///
/// `Handbook` is not here either, and that is the one place the two vocabularies
/// part: a handbook is what a project tells everybody who arrives, and a task
/// has no such thing. Offered on a task it would put an empty heading in the
/// combined edit buffer of every task in the store, for a section nobody could
/// sensibly fill.
pub const PROSE: [&str; 3] = ["Overview", "Details", "Decisions"];

/// The sections a person writes on a **project**: [`PROSE`] plus `Handbook`.
///
/// Two lists rather than one because the kinds genuinely differ, and one list
/// with a "not for tasks" rule beside it would be the same drift by another
/// name. Everything that renders or edits a project's prose reads this — the
/// detail pane included, which was carrying a fourth literal copy of the task
/// list and would otherwise have gone stale the moment this landed.
pub const PROJECT_PROSE: [&str; 4] = ["Overview", "Details", "Handbook", "Decisions"];

/// The headings a task or project body carries, in the order they are written.
///
/// `Overview` is what the thing is — written once, read to re-enter it later.
/// `Details` is working material: criteria, links, whatever the work needs.
/// `Handbook` is a project's standing answer to "I have just arrived here":
/// what the work is for, how it is done, and — the part that keeps it short —
/// *where the code's own documentation lives and what is in it*. A technical
/// map of a tree does not belong here. It belongs in the tree, versioned with
/// the code it describes, reviewed in the same diff, and put in front of
/// whoever changes that code; the same content held here drifts the moment
/// somebody refactors and nothing makes them look at it. So the handbook names
/// that file and says what it holds, and an agent that needs the map goes and
/// reads it. Projects only — see [`PROJECT_PROSE`].
/// `Decisions` is what was settled and now binds: dated, append-only, and not
/// to be edited, because a decision that can be quietly rewritten is not a
/// record of anything.
/// `Log` is the dated, append-only trail and is never edited by hand.
///
/// `Decisions` sits before `Log` deliberately. The log is where a thing has
/// been; a decision is a constraint on where it can go, which is worth reading
/// before the history rather than after it — and it keeps `Log` last, which is
/// what lets [`Task::log`] go on appending to the end of the body.
pub const SECTIONS: [&str; 5] = ["Overview", "Details", "Handbook", "Decisions", "Log"];

/// The lease a machine has when none is set: three beats of slack is 90 s, and
/// two to three minutes is what Ed asked for (agent-028).
pub const DEFAULT_LEASE: u32 = 180;

/// The one link mode there is.
pub const LINK_REQUIRED: &str = "required";

/// The word a group's `agent:` line uses to say wsp does not run it.
pub const MANUAL: &str = "manual";

/// What a new group is run on when nobody said and there is no group before
/// it to inherit from: the default kind, on whatever model the spawn itself
/// would choose. Written into the file rather than left implicit, so a reader
/// of the list sees that the group is run by wsp.
pub const DEFAULT_POLICY: &str = "claude";

/// A worklist's body sections. `Groups` is second because it is what the file
/// is for; `Overview` above it is where the first group's start condition
/// lives, there being no group before it to carry one.
pub const WORKLIST_SECTIONS: [&str; 4] = ["Overview", "Groups", "Decisions", "Log"];

/// The sections of a worklist a person writes by hand, which is every one of
/// them except the two nothing may type into.
///
/// `Groups` is out because the queue has verbs of its own and a window they may
/// edit it in — an editor that could rewrite the section would be a way around
/// both. `Log` is out for the reason it is out of [`PROSE`]: it is dated and
/// append-only, and editing history in place is how history stops being
/// evidence.
///
/// What is left is where the first group's start condition lives. There is no
/// barrier in front of group 1 to carry stop prose, so `## Overview` carries
/// it, which is what `wsp worklist edit` exists for.
pub const WORKLIST_PROSE: [&str; 2] = ["Overview", "Decisions"];
