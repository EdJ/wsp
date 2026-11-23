//! Subscribe (docs/state-core.md 4.3): `subscribe {}` sends the snapshot and
//! the id it is consistent at ([`Feed::State`]), then every event after it;
//! `subscribe {from}` sends every event after `from`, or is answered
//! `too_old`. Both then follow the head through `Events::wait_for`, in commit
//! order, reading with `events_after` from the last id sent, so no event is
//! skipped (2.3 case 5) and two subscribers see the same order. A
//! [`Feed::Beat`] (the head, and when) goes out every beat.
//!
//! [`open`] runs on the connection's thread, before the reply, so a refusal is
//! the reply; [`follow`] runs on a thread of its own after it, writing beside
//! the replies through the connection's shared writer. It ends when the
//! connection does, when the server stops, or when a write or the store fails
//! (and then it closes the connection: the client subscribes again from its
//! last id).

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

use wsp_api::{Failure, Feed, Frame, State, Subscribe, MAX_EVENTS, MAX_LINE};
use wsp_store_contract::{Error, EventId, SNAPSHOT_PART};

use crate::conn::{send, store_failure, Writer};
use crate::{now, Shared};

/// The longest one `wait_for` blocks, so that a closed connection or a
/// stopping server is seen promptly between beats.
const POLL: Duration = Duration::from_millis(200);

/// A subscription answered and not yet followed: the frames it starts with,
/// and the id it follows from.
pub(crate) struct Opened {
    first: Vec<Feed>,
    cursor: EventId,
}

/// Open `sub`: the snapshot, or the first page of the replay.
pub(crate) fn open(shared: &Shared, sub: &Subscribe) -> Result<Opened, Failure> {
    let store = &shared.store;
    match sub.from {
        None => {
            let (state, at) = store.snapshot().map_err(store_failure)?;
            Ok(Opened { first: snapshot_frames(state, at), cursor: at })
        }
        Some(from) => {
            let events = store.events_after(from, MAX_EVENTS as usize).map_err(store_failure)?;
            let cursor = events.last().map_or(from, |e| e.id);
            Ok(Opened { first: events.into_iter().map(Feed::Event).collect(), cursor })
        }
    }
}

/// Room left in a line for the frame around a snapshot (`{"feed":{"state":…,"at":N}}`).
const ENVELOPE: usize = 1024;

/// The frames that carry `state`: one [`Feed::State`] when it fits in a line,
/// as every client reads; else [`Feed::Part`]s of [`SNAPSHOT_PART`] bytes and a
/// closing [`Feed::State`] with the rest (wsp-702), so that no frame is past
/// [`MAX_LINE`] however large the store grows.
pub(crate) fn snapshot_frames(state: State, at: EventId) -> Vec<Feed> {
    if state.json_len() + ENVELOPE <= MAX_LINE {
        return vec![Feed::State { state, at }];
    }
    let mut pieces = state.split(SNAPSHOT_PART);
    let last = pieces.pop().unwrap_or_default();
    let mut frames: Vec<Feed> = pieces.into_iter().map(|part| Feed::Part { part, at }).collect();
    frames.push(Feed::State { state: last, at });
    frames
}

/// Write the subscription until it ends.
pub(crate) fn follow(shared: Arc<Shared>, writer: Writer, closed: Arc<AtomicBool>, opened: Opened) {
    let why = run(&shared, &writer, &closed, opened);
    if !closed.swap(true, Ordering::SeqCst) {
        writer.lock().unwrap_or_else(|e| e.into_inner()).shutdown();
        if let Some(why) = why {
            shared.log.line(&format!("a subscription ended: {why}"));
        }
    }
}

/// The loop; why it ended, when that is worth a log line.
fn run(shared: &Shared, writer: &Writer, closed: &AtomicBool, opened: Opened) -> Option<String> {
    let store = &shared.store;
    let beat = shared.cfg.beat;
    for feed in opened.first {
        if !send(writer, &Frame::Feed(feed)) {
            return None;
        }
    }
    let mut cursor = opened.cursor;
    let mut next_beat = Instant::now() + beat;
    let failed = |e: Error| Some(format!("the store failed: {e:?}"));
    loop {
        if closed.load(Ordering::SeqCst) || shared.stopping() {
            return None;
        }
        let now_i = Instant::now();
        if now_i >= next_beat {
            let head = match store.head() {
                Ok(h) => h,
                Err(e) => return failed(e),
            };
            if !send(writer, &Frame::Feed(Feed::Beat { beat: head, at: now() })) {
                return None;
            }
            next_beat = now_i + beat;
            continue;
        }
        let head = match store.wait_for(cursor, (next_beat - now_i).min(POLL)) {
            Ok(h) => h,
            Err(e) => return failed(e),
        };
        if head <= cursor {
            continue;
        }
        let events = match store.events_after(cursor, MAX_EVENTS as usize) {
            Ok(evs) => evs,
            Err(e) => return failed(e),
        };
        if events.is_empty() {
            // The head moved and nothing past the cursor is visible yet: a
            // backend that publishes the head first. Look again shortly.
            std::thread::sleep(Duration::from_millis(5));
            continue;
        }
        for e in events {
            cursor = e.id;
            if !send(writer, &Frame::Feed(Feed::Event(e))) {
                return None;
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::BTreeMap;
    use wsp_store_contract::{ProjectId, Row, RowId, RowState};

    fn state_of(rows: usize, each: usize) -> State {
        let mut s = State::default();
        for i in 0..rows {
            let id = RowId(format!("wsp-{i:04}"));
            let fields = BTreeMap::from([("body".to_string(), "x".repeat(each))]);
            let row = Row {
                id: id.clone(),
                project: ProjectId("wsp".into()),
                title: "t".into(),
                state: RowState::Done,
                before_block: None,
                seat: None,
                group: None,
                commit: None,
                fields,
            };
            s.rows.insert(id, row);
        }
        s
    }

    fn line_len(f: &Feed) -> usize {
        serde_json::to_vec(&Frame::Feed(f.clone())).unwrap().len()
    }

    #[test]
    fn a_snapshot_that_fits_a_line_is_one_state_and_one_past_it_is_parts_each_under_the_line() {
        // Under the line: one State, as a client older than Part reads it.
        let small = state_of(10, 1000);
        assert_eq!(snapshot_frames(small.clone(), EventId(7)), vec![Feed::State { state: small, at: EventId(7) }]);

        // wsp-702's store was 51 MB; 20 MB here, in rows of 100 kB.
        let big = state_of(200, 100_000);
        let frames = snapshot_frames(big.clone(), EventId(9));
        assert!(frames.len() >= 5, "{} frames", frames.len());
        let (last, parts) = frames.split_last().unwrap();
        assert!(matches!(last, Feed::State { at: EventId(9), .. }), "it closes with the State");
        assert!(parts.iter().all(|f| matches!(f, Feed::Part { at: EventId(9), .. })));
        let largest = frames.iter().map(line_len).max().unwrap();
        assert!(largest <= SNAPSHOT_PART + 200_000, "the largest frame is {largest} B");
        assert!(largest < MAX_LINE);
        let mut back = State::default();
        for f in frames {
            match f {
                Feed::Part { part, .. } | Feed::State { state: part, .. } => back.absorb(part),
                _ => unreachable!(),
            }
        }
        assert_eq!(back, big);
    }
}
