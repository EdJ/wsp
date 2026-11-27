//! The `wip` verb's reply, cached by event head (wsp-373).
//!
//! The census polls `wip --json` every few seconds, and the head rarely moves
//! between polls, but [`Reads::wip_snapshot`](wsp_store_contract::Reads) cuts
//! the whole state row each time (about 80 ms on a live store of 17.5 MB). So
//! the server keeps the cut's answer serialized, keyed by the head it is
//! consistent at: a `wip` while the head is the same is answered from those
//! bytes. The state row changes only by folding events, so the head names it.
//!
//! The cut is taken off the request path too ([`follow`]): the census reads
//! `wip --json` right after the head moves (host-core rings its gate on
//! `wsp stamp`), so a cache cut on request would make its reads the cold
//! ones. Once anything has asked for `wip`, a thread that follows the head
//! (through `wait_for`, as `revoke` and `subscribe` do) cuts afresh when the
//! head moves, and a read finds the cache warm. A
//! read that comes while that cut runs waits for it, and only a read at a
//! head the thread has not reached yet cuts for itself. A server nobody asks
//! `wip` of never cuts.
//!
//! It sits above the store, so every backend (the SQLite store, `MemStore`,
//! the hub's) is cached alike.
//!
//! A cut too long for one line (wsp-783: 26 MB on the live store, past the
//! 16 MiB [`MAX_LINE`]) is kept as its parts, each a [`SNAPSHOT_PART`] of the
//! state as `subscribe` splits a snapshot, each carrying how many there are.
//! `wip {}` is answered the first; the client asks the rest by the cut's head
//! (`wip { at, part }`), and the connection keeps the cut it began on until
//! its last part is read, so the head moving meanwhile does not tear it.

use std::sync::{Arc, Mutex, MutexGuard};
use std::time::Duration;

use wsp_api::{Answer, ReqId, MAX_LINE};
use wsp_store_contract::{EventId, Result, State, Store, SNAPSHOT_PART};

use crate::Shared;

/// One cut, serialised: the head it is consistent at, and the JSON of each
/// reply's `ok`, one per part (one alone for a cut that fits a line).
#[derive(Debug)]
pub(crate) struct Cut {
    pub(crate) at: EventId,
    pub(crate) parts: Vec<Vec<u8>>,
}

/// The last `wip` cut.
#[derive(Default)]
pub(crate) struct Cache(Mutex<Held>);

/// What the cache holds: the last cut, if any has been asked for.
type Held = Option<Arc<Cut>>;

/// Room left in a line for the reply around a cut's `ok`
/// (`{"type":"reply","id":…,"ok":…,"events":[]}` and its newline).
const SLACK: usize = 1024;

impl Cache {
    fn held(&self) -> MutexGuard<'_, Held> {
        self.0.lock().unwrap_or_else(|e| e.into_inner())
    }

    /// The cut a `wip` is answered from: the cached one while it is cut at
    /// the store's head (or past the head this call read), else a fresh cut,
    /// kept. Callers that arrive while a cut runs, theirs or [`follow`]'s,
    /// wait for it rather than cut again.
    pub(crate) fn answer(&self, store: &(dyn Store + Send + Sync)) -> Result<Arc<Cut>> {
        let mut held = self.held();
        let head = store.head()?;
        if let Some(cut) = held.as_ref() {
            if cut.at >= head {
                return Ok(cut.clone());
            }
        }
        cut(store, &mut held)
    }

    /// The cut held, when it is the one at `at`: what a part is read from on
    /// a connection that did not begin on it.
    pub(crate) fn at(&self, at: EventId) -> Option<Arc<Cut>> {
        self.held().as_ref().filter(|c| c.at == at).cloned()
    }

    /// Cut afresh when something has asked for `wip` and the cut held is
    /// behind the store's head; `true` when it cut.
    pub(crate) fn refresh(&self, store: &(dyn Store + Send + Sync)) -> Result<bool> {
        let mut held = self.held();
        let Some(cut) = held.as_ref() else { return Ok(false) };
        if cut.at >= store.head()? {
            return Ok(false);
        }
        self::cut(store, &mut held).map(|_| true)
    }
}

/// Cut the state row for `wip`, serialise the answer, and keep it in `held`.
fn cut(store: &(dyn Store + Send + Sync), held: &mut Held) -> Result<Arc<Cut>> {
    let (state, at) = store.wip_snapshot()?;
    let cut = Arc::new(Cut { at, parts: parts(state, at)? });
    // Keyed by the cut's own head: an event folded between the head read and
    // the cut leaves the cache one behind, so the next call cuts again.
    *held = Some(cut.clone());
    Ok(cut)
}

/// The `ok` of each reply that carries `state`: one [`Answer::Wip`] with no
/// `parts` when its line fits under [`MAX_LINE`], the bytes it always was;
/// else one per [`SNAPSHOT_PART`] piece of it, each naming how many there
/// are, so that no line is past the limit however large the store grows.
pub(crate) fn parts(state: State, at: EventId) -> Result<Vec<Vec<u8>>> {
    let json = |answer: &Answer| {
        serde_json::to_vec(answer).map_err(|e| wsp_store_contract::Error::Backend {
            reason: format!("the wip answer did not serialise: {e}"),
        })
    };
    if state.json_len() + SLACK <= MAX_LINE {
        return Ok(vec![json(&Answer::Wip { state, at, parts: None })?]);
    }
    let pieces = state.split(SNAPSHOT_PART);
    let n = Some(pieces.len() as u32);
    pieces.into_iter().map(|state| json(&Answer::Wip { state, at, parts: n })).collect()
}

/// How often [`follow`] looks at `stopping`, and how long it backs off when the store fails.
const POLL: Duration = Duration::from_millis(100);

/// Keep the cache warm until the server stops: each time the head moves, cut
/// afresh at once, so the census's read just after a change finds the cut
/// made or under way. A burst is not cut once per event: the head is read
/// again only after a cut, so whatever folded while it ran is one next cut.
/// (A settling wait was tried and dropped: on the live store a fold takes
/// 55 ms or more, longer than any wait worth the delay it adds to every read.)
/// Nothing is cut until a `wip` has been asked once.
pub(crate) fn follow(shared: Arc<Shared>) {
    let store = shared.store.as_ref();
    let mut cursor = loop {
        match store.head() {
            Ok(h) => break h,
            Err(_) if shared.stopping() => return,
            Err(_) => std::thread::sleep(POLL),
        }
    };
    let mut failing = false;
    while !shared.stopping() {
        let head = match store.wait_for(cursor, POLL) {
            Ok(h) => h,
            Err(_) => {
                std::thread::sleep(POLL);
                continue;
            }
        };
        if head <= cursor {
            continue;
        }
        cursor = head;
        match shared.wip.refresh(store) {
            Ok(_) => failing = false,
            Err(e) => {
                if !failing {
                    shared.log.line(&format!("the wip cache was not refreshed: {e:?}"));
                }
                failing = true;
                std::thread::sleep(POLL);
            }
        }
    }
}

/// The reply line to request `id` whose `ok` is `ok` (JSON), newline ended:
/// byte for byte what [`Frame::Reply`] with that answer and no events
/// serialises to, without serialising the answer again.
pub(crate) fn line(id: ReqId, ok: &[u8]) -> Vec<u8> {
    let head = format!("{{\"type\":\"reply\",\"id\":{},\"ok\":", id.0);
    const TAIL: &[u8] = b",\"events\":[]}\n";
    let mut out = Vec::with_capacity(head.len() + ok.len() + TAIL.len());
    out.extend_from_slice(head.as_bytes());
    out.extend_from_slice(ok);
    out.extend_from_slice(TAIL);
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use wsp_api::{Frame, Outcome, Reply};
    use wsp_store_conformance::MemStore;
    use wsp_store_contract::Reads;

    /// The spliced line is the frame serde writes, so a client cannot tell a
    /// cached answer from a cut one. The cache itself is pinned over the wire,
    /// in tests/server.rs.
    #[test]
    fn the_line_is_the_reply_frame_byte_for_byte() {
        let store = MemStore::new();
        let (state, at) = store.wip_snapshot().unwrap();
        let answer = Answer::Wip { state, at, parts: None };
        let ok = serde_json::to_vec(&answer).unwrap();
        for id in [0, 7, u64::MAX] {
            let frame = Frame::Reply(Reply { id: ReqId(id), outcome: Outcome::Ok(answer.clone()), events: vec![] });
            let mut want = serde_json::to_vec(&frame).unwrap();
            want.push(b'\n');
            assert_eq!(line(ReqId(id), &ok), want, "id {id}");
        }
    }
}
