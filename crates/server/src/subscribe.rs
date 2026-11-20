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

use wsp_api::{Failure, Feed, Frame, Subscribe, MAX_EVENTS};
use wsp_store_contract::{Error, EventId};

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
            Ok(Opened { first: vec![Feed::State { state, at }], cursor: at })
        }
        Some(from) => {
            let events = store.events_after(from, MAX_EVENTS as usize).map_err(store_failure)?;
            let cursor = events.last().map_or(from, |e| e.id);
            Ok(Opened { first: events.into_iter().map(Feed::Event).collect(), cursor })
        }
    }
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
