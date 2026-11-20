//! Subscribe, v1 (docs/state-core.md section 4.3): the frames a subscriber
//! receives. It sits on [`Events::events_after`](crate::Events::events_after)
//! and [`Events::wait_for`](crate::Events::wait_for); event ids are the
//! de-duplication key, so delivery is at-least-once.

use serde::{Deserialize, Serialize};

use crate::entity::State;
use crate::event::Event;
use crate::id::*;

/// Seconds between heartbeats.
pub const BEAT_SECS: u64 = 10;

/// Heartbeats a subscriber may miss before it reconnects.
pub const MISSED_BEATS: u32 = 3;

/// `subscribe { from? }`: from an id, every event after it; absent, a snapshot
/// first and then every event after the snapshot's id.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Subscribe {
    #[serde(default)]
    pub from: Option<EventId>,
}

/// One frame of a subscription.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Feed {
    /// The snapshot `subscribe {}` sends first, and the id it is consistent at.
    State { state: State, at: EventId },
    /// One event, in commit order.
    Event(Event),
    /// The heartbeat: the head's id, and when.
    Beat { beat: EventId, at: At },
}
