//! Subscribe, v1 (docs/state-core.md section 4.3): the frames a subscriber
//! receives. It sits on [`Events::events_after`](crate::Events::events_after)
//! and [`Events::wait_for`](crate::Events::wait_for); event ids are the
//! de-duplication key, so delivery is at-least-once.

use serde::{Deserialize, Serialize};

use crate::entity::State;
use crate::event::{Event, Subject};
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

/// The most a [`Feed::Part`] or the closing [`Feed::State`] of a snapshot is
/// filled to, in bytes of JSON: a quarter of the API's 16 MiB line, so that a
/// frame stays under it with room for the one entity that overruns a part.
pub const SNAPSHOT_PART: usize = 4 * 1024 * 1024;

/// One frame of a subscription.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Feed {
    /// The snapshot `subscribe {}` sends first, and the id it is consistent at.
    /// A snapshot too large for one frame comes as [`Feed::Part`]s and closes
    /// with this frame, which holds the rest: the snapshot is all of them
    /// [`absorbed`](State::absorb) in order.
    State { state: State, at: EventId },
    /// A piece of a snapshot that a [`Feed::State`] closes (wsp-702). Pieces
    /// are disjoint: each entity is in one of them.
    Part { part: State, at: EventId },
    /// A fresh snapshot on a live subscription (wsp-174): the subscriber's
    /// scope changed (it was added to a workspace, or removed from one), so
    /// the state it may read is sent again at the current head. Like
    /// [`Feed::State`] it may be preceded by [`Feed::Part`]s, and a client
    /// folds it as it folds a first snapshot: what it held is replaced.
    Resync { state: State, at: EventId },
    /// A record left the subscriber's sight (wsp-174): the event `at` moved
    /// `subject` out of every workspace the subscriber may read, so its fold
    /// drops it. `at` is the event's id, so a gap in ids stays a gap.
    Gone { subject: Subject, at: EventId },
    /// One event, in commit order.
    Event(Event),
    /// The heartbeat: the head's id, and when.
    Beat { beat: EventId, at: At },
}

impl State {
    /// Take `other`'s entities into this one: how the pieces of a split
    /// snapshot are put back together. Runs are appended, so pieces taken in
    /// the order they were sent keep the runs' order.
    pub fn absorb(&mut self, other: State) {
        let State { rows, lists, seats, claims, orders, asks, mut runs, machines, leases, users, credentials, workspaces, members } = other;
        self.rows.extend(rows);
        self.lists.extend(lists);
        self.seats.extend(seats);
        self.claims.extend(claims);
        self.orders.extend(orders);
        self.asks.extend(asks);
        self.runs.append(&mut runs);
        self.machines.extend(machines);
        self.leases.extend(leases);
        self.users.extend(users);
        self.credentials.extend(credentials);
        self.workspaces.extend(workspaces);
        self.members.extend(members);
    }

    /// The length of this state's JSON, counted without writing it out.
    pub fn json_len(&self) -> usize {
        json_len(self)
    }

    /// Split into pieces of about `budget` bytes of JSON each, in the order
    /// [`absorb`](State::absorb) puts them back: never empty (a small state is
    /// one piece, itself), and an entity larger than `budget` is a piece alone.
    pub fn split(mut self, budget: usize) -> Vec<State> {
        let mut pieces = Vec::new();
        let mut cur = State::default();
        let mut size = 0usize;
        // Close the piece being filled when `n` more bytes would overrun it.
        let mut room = |n: usize, cur: &mut State, pieces: &mut Vec<State>| {
            if size > 0 && size + n > budget {
                pieces.push(std::mem::take(cur));
                size = 0;
            }
            size += n;
        };
        macro_rules! take {
            ($field:ident) => {
                for (k, v) in std::mem::take(&mut self.$field) {
                    room(json_len(&k) + json_len(&v) + 2, &mut cur, &mut pieces);
                    cur.$field.insert(k, v);
                }
            };
        }
        take!(rows);
        take!(lists);
        take!(seats);
        take!(claims);
        take!(orders);
        take!(asks);
        for run in std::mem::take(&mut self.runs) {
            room(json_len(&run) + 1, &mut cur, &mut pieces);
            cur.runs.push(run);
        }
        take!(machines);
        take!(leases);
        take!(users);
        take!(credentials);
        take!(workspaces);
        take!(members);
        pieces.push(cur);
        pieces
    }
}

/// The length of a value's JSON, counted without keeping it.
fn json_len<T: Serialize>(v: &T) -> usize {
    struct Count(usize);
    impl std::io::Write for Count {
        fn write(&mut self, b: &[u8]) -> std::io::Result<usize> {
            self.0 += b.len();
            Ok(b.len())
        }
        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }
    let mut n = Count(0);
    let _ = serde_json::to_writer(&mut n, v);
    n.0
}
