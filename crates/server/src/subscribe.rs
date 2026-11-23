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

use std::collections::BTreeSet;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

use wsp_api::{Failure, Feed, Frame, Principal, State, Subscribe, MAX_EVENTS, MAX_LINE};
use wsp_store_contract::{Entity, Error, Event, EventId, EventKind, SharedName, Subject, WorkspaceId, SNAPSHOT_PART};

use crate::conn::{send, store_failure, Writer};
use crate::mirror::entity_at;
use crate::{now, Shared};

/// The longest one `wait_for` blocks, so that a closed connection or a
/// stopping server is seen promptly between beats.
const POLL: Duration = Duration::from_millis(200);

/// A subscription answered and not yet followed: the frames it starts with,
/// the id it follows from, and what it may see.
pub(crate) struct Opened {
    first: Vec<Feed>,
    cursor: EventId,
    watch: Watch,
}

/// What one subscription may read, and what it has been sent (wsp-174). A
/// server with no gate reads everything (`sight` is none) and this does
/// nothing. With one, the subscription holds only the records of the shared
/// workspaces the gate says its principal may read:
///
/// - an event goes out when its `workspace` is one of them;
/// - an event that puts outside them a record the subscriber holds (it was
///   sent, in the snapshot or by an event) goes out as [`Feed::Gone`] instead,
///   so the client's fold drops the record;
/// - an event that puts inside them a record the subscriber never saw (it
///   moved in) goes out with the record's whole entity, as a `mirror.put`;
/// - an event of the identity (a member or a workspace) is read for what it
///   does to the scope: when the principal's workspaces changed, the state it
///   may read is sent again at the head ([`Feed::Resync`]).
///
/// The server learns no selector of the client's: the workspaces are the
/// domain's own, and the principal's the hello's.
struct Watch {
    by: Principal,
    sight: Option<BTreeSet<SharedName>>,
    /// The records the subscriber holds, by subject (rows, lists, orders, asks).
    seen: BTreeSet<Subject>,
    /// The head the scope was read at. An event at or before it is a replay
    /// (`subscribe {from}`): the scope the subscriber had at `from` is not
    /// kept, so an event that could have changed it is answered with a
    /// [`Feed::Resync`] at the head instead of being replayed.
    replay_to: EventId,
}

/// What the events read so far leave to send.
struct Sent {
    frames: Vec<Feed>,
    cursor: EventId,
}

/// Open `sub` for `by`: the snapshot, or the first page of the replay.
pub(crate) fn open(shared: &Shared, sub: &Subscribe, by: &Principal) -> Result<Opened, Failure> {
    let store = &shared.store;
    let gate = shared.cfg.gate.clone();
    match sub.from {
        None => {
            let (state, at) = store.snapshot().map_err(store_failure)?;
            let sight = gate.and_then(|g| g.readable(by, &state));
            let mut watch = Watch { by: by.clone(), sight, seen: BTreeSet::new(), replay_to: EventId(0) };
            let state = watch.scope(state);
            Ok(Opened { first: snapshot_frames(state, at), cursor: at, watch })
        }
        Some(from) => {
            let events = store.events_after(from, MAX_EVENTS as usize).map_err(store_failure)?;
            let mut watch = Watch { by: by.clone(), sight: None, seen: BTreeSet::new(), replay_to: EventId(0) };
            if let Some(gate) = gate {
                let (state, at) = store.snapshot().map_err(store_failure)?;
                watch.sight = gate.readable(by, &state);
                watch.replay_to = at;
                watch.scope(state);
            }
            let cursor = events.last().map_or(from, |e| e.id);
            let sent = watch.read(shared, events, from).map_err(store_failure)?;
            Ok(Opened { first: sent.frames, cursor: sent.cursor.max(cursor), watch })
        }
    }
}

impl Watch {
    /// `state` as this subscription may read it, and the records it holds
    /// noted. Unscoped, it is `state` as it stands.
    fn scope(&mut self, mut state: State) -> State {
        self.seen.clear();
        let Some(sight) = &self.sight else { return state };
        let allows = |w: &WorkspaceId| matches!(w, WorkspaceId::Shared(n) if sight.contains(n));
        state.rows.retain(|_, r| allows(&r.workspace));
        state.lists.retain(|_, l| allows(&l.workspace));
        state.orders.retain(|_, o| allows(&o.workspace));
        state.asks.retain(|_, a| allows(&a.workspace));
        // A claim is its row's; the rest of the machine's and the identity's
        // records name no workspace and are no subscriber's to read.
        let rows = &state.rows;
        state.claims.retain(|row, _| rows.contains_key(row));
        state.seats.clear();
        state.runs.clear();
        state.machines.clear();
        state.leases.clear();
        state.users.clear();
        state.credentials.clear();
        state.workspaces.retain(|n, _| sight.contains(n));
        state.members.retain(|n, _| sight.contains(n));
        self.seen.extend(state.rows.keys().cloned().map(Subject::Row));
        self.seen.extend(state.lists.keys().cloned().map(Subject::List));
        self.seen.extend(state.orders.keys().cloned().map(Subject::Order));
        self.seen.extend(state.asks.keys().cloned().map(Subject::Ask));
        state
    }

    /// The frames for `events`, in order, and the id the subscription is at
    /// after them (`cursor` when it sent none). A scope change ends the batch:
    /// the resync is at the head, which has what the rest of it held.
    fn read(&mut self, shared: &Shared, events: Vec<Event>, cursor: EventId) -> Result<Sent, Error> {
        let mut sent = Sent { frames: Vec::new(), cursor };
        // A replay that could not be told from the scope it left: the fresh
        // snapshot, sent last (the client drops what is at or before it).
        let mut resync: Option<(Vec<Feed>, EventId)> = None;
        for e in events {
            let id = e.id;
            if self.sight.is_none() {
                sent.frames.push(Feed::Event(e));
                sent.cursor = id;
                continue;
            }
            if resync.as_ref().is_some_and(|(_, at)| id > *at) {
                break;
            }
            let identity = matches!(e.subject, Subject::Member { .. } | Subject::Workspace(_));
            // A replay cannot say what the scope was at `from`: an identity
            // event, or a held record's event outside the scope the head
            // gives (it may have just left), is a resync at the head.
            let replaying = id <= self.replay_to;
            let unknown = replaying && (identity || (self.is_held(&e) && !self.reads(&e)));
            if resync.is_none() && (identity || unknown) {
                if let Some((frames, at)) = self.rescope(shared, unknown)? {
                    if !replaying {
                        // The resync is the head, which has what the frames before it said.
                        sent.frames = frames;
                        sent.cursor = at;
                        return Ok(sent);
                    }
                    // What is not a held record's (a ref moving, a lease) is not in
                    // the snapshot: it is still sent, before it.
                    sent.frames.retain(|f| !self.is_held_frame(f));
                    resync = Some((frames, at));
                }
            }
            sent.cursor = id;
            if resync.is_some() && (identity || self.is_held(&e)) {
                continue;
            }
            if let Some(frame) = self.admit(shared, e)? {
                sent.frames.push(frame);
            }
        }
        if let Some((frames, at)) = resync {
            sent.frames.extend(frames);
            sent.cursor = at;
        }
        Ok(sent)
    }

    /// Whether `f` says something of a record the subscriber holds.
    fn is_held_frame(&self, f: &Feed) -> bool {
        match f {
            Feed::Gone { .. } => true,
            Feed::Event(e) => self.is_held(e),
            _ => false,
        }
    }

    /// The fresh snapshot, and the id it is at, when the principal's
    /// workspaces are not what they were.
    fn rescope(&mut self, shared: &Shared, force: bool) -> Result<Option<(Vec<Feed>, EventId)>, Error> {
        let Some(gate) = &shared.cfg.gate else { return Ok(None) };
        let (state, at) = shared.store.snapshot()?;
        let now = gate.readable(&self.by, &state);
        if now == self.sight && !force {
            return Ok(None);
        }
        self.sight = now;
        let state = self.scope(state);
        let mut frames = snapshot_frames(state, at);
        if let Some(Feed::State { state, at }) = frames.pop() {
            frames.push(Feed::Resync { state, at });
        }
        Ok(Some((frames, at)))
    }

    /// Whether `e` is in a workspace this subscription may read.
    fn reads(&self, e: &Event) -> bool {
        matches!((&self.sight, &e.workspace), (Some(sight), WorkspaceId::Shared(n)) if sight.contains(n))
    }

    /// Whether `e` is an event of a record the subscriber holds whole.
    fn is_held(&self, e: &Event) -> bool {
        matches!(e.subject, Subject::Row(_) | Subject::List(_) | Subject::Order(_) | Subject::Ask(_))
            && e.kind != EventKind::RefMoved
    }

    /// What `e` is to this subscription: itself, a `gone`, the record whole, or nothing.
    fn admit(&mut self, shared: &Shared, e: Event) -> Result<Option<Feed>, Error> {
        if self.sight.is_none() {
            return Ok(Some(Feed::Event(e)));
        }
        let inside = self.reads(&e);
        let held = self.is_held(&e);
        Ok(match (inside, held) {
            (true, false) => Some(Feed::Event(e)),
            (true, true) => {
                let new = self.seen.insert(e.subject.clone());
                if !new || creates(e.kind) {
                    return Ok(Some(Feed::Event(e)));
                }
                // A record the subscriber never saw has moved in: send it whole.
                let (state, _) = shared.store.snapshot()?;
                entity_at(&state, &e.subject).and_then(|entity| whole(e, &entity))
            }
            (false, true) => {
                self.seen.remove(&e.subject).then_some(Feed::Gone { subject: e.subject, at: e.id })
            }
            (false, false) => None,
        })
    }
}

/// Whether `kind` brings a record into being, which a subscriber who sees the
/// record's workspace is sent as it stands.
fn creates(kind: EventKind) -> bool {
    match serde_json::to_value(kind) {
        Ok(serde_json::Value::String(name)) => name == "mirror.put" || name.ends_with(".added"),
        _ => false,
    }
}

/// `e` as a `mirror.put` of the whole `entity`.
fn whole(e: Event, entity: &Entity) -> Option<Feed> {
    let payload = serde_json::to_value(entity).ok()?;
    Some(Feed::Event(Event { kind: EventKind::MirrorPut, payload, ..e }))
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
    let mut watch = opened.watch;
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
        let sent = match watch.read(shared, events, cursor) {
            Ok(sent) => sent,
            Err(e) => return failed(e),
        };
        cursor = sent.cursor;
        for frame in sent.frames {
            if !send(writer, &Frame::Feed(frame)) {
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
                workspace: Default::default(),
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
