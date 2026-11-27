//! Subscribe to the hub from the spoke's side (docs/state-core.md 4.3): at
//! least once, de-duplicated by event id, with a reconnect of its own.
//!
//! A [`Feed`] owns a connection of its own (the server allows one subscription
//! to a connection). [`Feed::next_within`] hands out [`Item`]s in order: the
//! snapshot (`subscribe {}`), then each event once. A beat resets the count of
//! missed ones; [`MISSED_BEATS`] quiet beats in a row, or a closed connection,
//! and the feed dials again and subscribes `from` its last id. That is
//! announced as one [`Item::Resumed`] so that a caller with derived state knows
//! to re-read. A `too_old` answer leads to `subscribe {}` and a fresh
//! [`Item::State`]. Ids at or before the last handed out are dropped.
//!
//! On a scoped subscription ids have gaps: the server sends only
//! the events of the workspaces the subscriber may read, and nothing here
//! assumes the ids are contiguous, only that they grow. A [`Item::Gone`]
//! drops a record that left the subscriber's sight; an [`Item::Resync`] is a
//! fresh snapshot after a change of scope, folded as the first one.
//!
//! A snapshot too large for one line comes as `Feed::Part`s closed by a
//! `Feed::State` (wsp-702); the feed puts them back together and hands out
//! one [`Item::State`], as for a snapshot that came whole. A connection lost
//! before it delivered anything (no item, no beat) is dialled again after a
//! backoff that doubles each time, as a failed dial is, so that a snapshot the
//! client cannot read does not cost the server one every 25 ms.
//!
//! A `refused { unauthorised }` after a welcome is final (wsp-162): the hub
//! sends it to a subscriber whose token was revoked, then closes. The feed
//! hands out what it held, dials no more, and [`Feed::refused`] says why; its
//! iterator ends. A redial whose hello is refused `unauthorised` ends it too.

use std::collections::VecDeque;
use std::time::{Duration, Instant};

use wsp_hub_protocol::{
    Answer, Call, Code, ConnError, Connection, Event, EventId, Feed as Frames, Frame, Outcome, ReqId, Request, State,
    Subject, Subscribe, Verb, BEAT_SECS, MISSED_BEATS, PROTOCOL,
};

use super::dial::{self, Silent, Tcp};
use super::Dial;

/// What a [`Feed`] hands out.
#[derive(Debug, Clone, PartialEq)]
pub enum Item {
    /// The snapshot, and the id it is consistent at: the first item of
    /// `subscribe(None)`, and the one after a `too_old`.
    State { state: State, at: EventId },
    /// A fresh snapshot of a live subscription (wsp-174): the subscriber's
    /// scope changed, so what it may read is sent again at the head. Fold it
    /// as the first [`Item::State`]: what was held is replaced.
    Resync { state: State, at: EventId },
    /// A record left the subscriber's sight (wsp-174): drop `subject`. `at` is
    /// the id of the event that moved it.
    Gone { subject: Subject, at: EventId },
    /// One event, in commit order; no id is handed out twice.
    Event(Event),
    /// The feed lost its connection and has a new one, subscribed from `from`
    /// (the last id handed out). Events missed meanwhile follow.
    Resumed { from: EventId },
}

/// An iterator over a subscription. See the module docs.
pub struct Feed {
    target: Dial,
    timeout: Duration,
    beat: Duration,
    conn: Option<Tcp>,
    /// The last event id (or snapshot id) handed out.
    last: Option<EventId>,
    pending: VecDeque<Item>,
    next_req: u64,
    /// When a frame was last sent: the server drops a connection it hears nothing
    /// on for `MISSED_BEATS` beats, so a feed asks for nothing once a beat.
    sent: Instant,
    /// When a frame last came, and the quiet beats counted since.
    heard: Instant,
    quiet: u32,
    missed: u64,
    beats: u64,
    dropped: u64,
    reconnects: u64,
    connected_before: bool,
    /// Whether this connection has delivered an item or a beat: a loss before
    /// it does backs off.
    delivered: bool,
    /// The pieces of a split snapshot so far, and the id they are at.
    parts: Option<(State, EventId)>,
    silent: Option<String>,
    /// Why the feed is over for good: the server refused its token.
    refused: Option<String>,
    backoff: Duration,
    retry_at: Instant,
}

const BACKOFF_FIRST: Duration = Duration::from_millis(25);
const BACKOFF_MAX: Duration = Duration::from_secs(1);

impl Feed {
    pub(crate) fn new(target: Dial, from: Option<EventId>, timeout: Duration) -> Feed {
        Feed {
            target,
            timeout,
            beat: Duration::from_secs(BEAT_SECS),
            conn: None,
            last: from,
            pending: VecDeque::new(),
            next_req: 1,
            sent: Instant::now(),
            heard: Instant::now(),
            quiet: 0,
            missed: 0,
            beats: 0,
            dropped: 0,
            reconnects: 0,
            connected_before: false,
            delivered: false,
            parts: None,
            silent: None,
            refused: None,
            backoff: BACKOFF_FIRST,
            retry_at: Instant::now(),
        }
    }

    /// The server's beat period, [`BEAT_SECS`] unless a server is configured
    /// otherwise (tests). Missed beats are counted against it.
    pub fn set_beat(&mut self, beat: Duration) {
        self.beat = beat.max(Duration::from_millis(1));
    }

    /// Beats that did not come, in all.
    pub fn missed_beats(&self) -> u64 {
        self.missed
    }

    /// Beats that came.
    pub fn beats(&self) -> u64 {
        self.beats
    }

    /// Event ids dropped because they had been handed out already.
    pub fn duplicates(&self) -> u64 {
        self.dropped
    }

    /// Times the feed dialled again after losing its connection.
    pub fn reconnects(&self) -> u64 {
        self.reconnects
    }

    /// The last id handed out (an event's, or the snapshot's).
    pub fn last_id(&self) -> Option<EventId> {
        self.last
    }

    /// Why the last attempt to connect failed, while the feed is not connected.
    pub fn why_silent(&self) -> Option<&str> {
        self.silent.as_deref()
    }

    /// Why the feed ended for good, once the server refused its token
    /// `unauthorised` after welcoming it: it does not dial again.
    pub fn refused(&self) -> Option<&str> {
        self.refused.as_deref()
    }

    /// The next item, waiting up to `wait` for it; `None` when none came (the
    /// feed keeps trying to connect in the meantime and again on the next call).
    pub fn next_within(&mut self, wait: Duration) -> Option<Item> {
        let deadline = Instant::now() + wait;
        loop {
            if let Some(item) = self.pending.pop_front() {
                return Some(item);
            }
            if self.refused.is_some() {
                return None;
            }
            let left = deadline.saturating_duration_since(Instant::now());
            if self.conn.is_none() {
                let now = Instant::now();
                if now < self.retry_at {
                    let pause = (self.retry_at - now).min(left);
                    if pause.is_zero() {
                        return None;
                    }
                    std::thread::sleep(pause);
                    continue;
                }
                if left.is_zero() {
                    return None;
                }
                self.connect();
                continue;
            }
            if left.is_zero() {
                return None;
            }
            if self.sent.elapsed() >= self.beat {
                self.keep_alive();
                continue;
            }
            let now = Instant::now();
            let until_drop = (self.heard + self.beat * MISSED_BEATS).saturating_duration_since(now);
            let until_ask = (self.sent + self.beat).saturating_duration_since(now);
            let step = left.min(until_drop).min(until_ask).max(Duration::from_millis(1));
            let got = self.conn.as_mut().expect("connected").recv(step);
            match got {
                Ok(Some(Frame::Feed(frame))) => self.take(frame),
                Ok(Some(Frame::Refused(r))) if r.code == Code::Unauthorised => self.end(r.text),
                Ok(Some(_)) => {}
                Ok(None) => self.quiet_wake(),
                Err(e) => self.lose(format!("the connection failed: {e}")),
            }
        }
    }

    /// The cheap read of the server's contract (4.3): `events` after the last id,
    /// no more than none. Its reply is read past; it only has to be heard.
    fn keep_alive(&mut self) {
        let from = self.last.unwrap_or(EventId(0));
        let id = ReqId(self.next_req);
        self.next_req += 1;
        let call = Call::Verb(Verb::Events { from, limit: Some(0) });
        let request = Request { v: PROTOCOL, id, key: None, by: self.target.by.clone(), call };
        self.sent = Instant::now();
        let sent = self.conn.as_mut().expect("connected").send(&Frame::Request(request));
        if let Err(e) = sent {
            self.lose(format!("the connection failed: {e}"));
        }
    }

    fn take(&mut self, frame: Frames) {
        self.heard = Instant::now();
        self.quiet = 0;
        match frame {
            Frames::Beat { .. } => {
                self.beats += 1;
                self.delivered = true;
            }
            Frames::Part { part, at } => match &mut self.parts {
                Some((held, held_at)) if *held_at == at => held.absorb(part),
                _ => self.parts = Some((part, at)),
            },
            Frames::State { state, at } => {
                let state = match self.parts.take() {
                    Some((mut held, held_at)) if held_at == at => {
                        held.absorb(state);
                        held
                    }
                    _ => state,
                };
                self.last = Some(at);
                self.delivered = true;
                self.pending.push_back(Item::State { state, at });
            }
            Frames::Resync { state, at } => {
                let state = match self.parts.take() {
                    Some((mut held, held_at)) if held_at == at => {
                        held.absorb(state);
                        held
                    }
                    _ => state,
                };
                // A snapshot is not an event: it is handed out whatever the
                // last id was, which scoped feeds leave with gaps.
                self.last = Some(at);
                self.delivered = true;
                self.pending.push_back(Item::Resync { state, at });
            }
            Frames::Gone { subject, at } => {
                self.delivered = true;
                if self.last.is_some_and(|last| at <= last) {
                    self.dropped += 1;
                } else {
                    self.last = Some(at);
                    self.pending.push_back(Item::Gone { subject, at });
                }
            }
            Frames::Event(e) => {
                self.delivered = true;
                if self.last.is_some_and(|last| e.id <= last) {
                    self.dropped += 1;
                } else {
                    self.last = Some(e.id);
                    self.pending.push_back(Item::Event(e));
                }
            }
        }
    }

    /// A `recv` that came back quiet: count the beats that did not come.
    fn quiet_wake(&mut self) {
        let silent = self.heard.elapsed();
        let beats = (silent.as_millis() / self.beat.as_millis()).min(u128::from(u32::MAX)) as u32;
        if beats > self.quiet {
            self.missed += u64::from(beats - self.quiet);
            self.quiet = beats;
        }
        if self.quiet >= MISSED_BEATS {
            self.lose(format!("{} beats missed", self.quiet));
        }
    }

    fn lose(&mut self, why: String) {
        if let Some(c) = self.conn.take() {
            c.shutdown();
        }
        self.parts = None;
        if self.delivered {
            self.silent = Some(why);
            self.backoff = BACKOFF_FIRST;
            self.retry_at = Instant::now();
        } else {
            // It subscribed and gave nothing: dialling again at once would ask
            // the same of the server, as fast as it answers.
            self.failed(why);
        }
    }

    /// The server refused the token after welcoming it: no more dials.
    fn end(&mut self, why: String) {
        if let Some(c) = self.conn.take() {
            c.shutdown();
        }
        self.parts = None;
        self.silent = Some(format!("refused unauthorised: {why}"));
        self.refused = Some(why);
    }

    fn failed(&mut self, why: String) {
        self.silent = Some(why);
        self.retry_at = Instant::now() + self.backoff;
        self.backoff = (self.backoff * 2).min(BACKOFF_MAX);
    }

    /// Dial and subscribe: `from` the last id, or `{}` at the first connection
    /// of a `subscribe(None)`, or again `{}` after `too_old`.
    fn connect(&mut self) {
        let mut open = match dial::open(&self.target) {
            Ok(o) => o,
            Err(Silent::Refused(r)) if r.code == Code::Unauthorised && self.connected_before => {
                return self.end(r.text);
            }
            Err(s) => return self.failed(s.to_string()),
        };
        let resumed = self.connected_before.then_some(self.last).flatten();
        let mut asked = self.last;
        loop {
            match self.subscribe_on(&mut open.conn, asked) {
                Ok(Answer::Subscribed) => break,
                Ok(other) => return self.reject(open.conn, format!("subscribe was answered {other:?}")),
                Err(Refusal::TooOld) if asked.is_some() => {
                    asked = None;
                    // The fresh State replaces what was held: what follows is
                    // counted from its id.
                    self.last = None;
                }
                Err(Refusal::TooOld) => return self.reject(open.conn, "subscribe {} was refused too_old".into()),
                Err(Refusal::Other(why)) => return self.reject(open.conn, why),
                Err(Refusal::Revoked(why)) => {
                    open.conn.shutdown();
                    return self.end(why);
                }
            }
        }
        let reconnect = self.connected_before;
        self.connected_before = true;
        if reconnect {
            self.reconnects += 1;
        }
        if let Some(from) = resumed {
            self.pending.push_back(Item::Resumed { from });
        }
        self.conn = Some(open.conn);
        self.silent = None;
        self.sent = Instant::now();
        self.heard = Instant::now();
        self.quiet = 0;
        self.delivered = false;
        self.parts = None;
    }

    fn reject(&mut self, conn: Tcp, why: String) {
        conn.shutdown();
        self.failed(why);
    }

    /// One `subscribe` request and its reply.
    fn subscribe_on(&mut self, conn: &mut Tcp, from: Option<EventId>) -> Result<Answer, Refusal> {
        let id = ReqId(self.next_req);
        self.next_req += 1;
        let call = Call::Verb(Verb::Subscribe(Subscribe { from }));
        let request = Request { v: PROTOCOL, id, key: None, by: self.target.by.clone(), call };
        conn.send(&Frame::Request(request)).map_err(|e| Refusal::Other(e.to_string()))?;
        let deadline = Instant::now() + self.timeout;
        loop {
            let left = deadline.saturating_duration_since(Instant::now());
            match conn.recv(left) {
                Ok(Some(Frame::Reply(r))) if r.id == id => {
                    return match r.outcome {
                        Outcome::Ok(a) => Ok(a),
                        Outcome::Err(f) if f.code == Code::TooOld => Err(Refusal::TooOld),
                        Outcome::Err(f) => Err(Refusal::Other(format!("subscribe was refused: {}", super::words::failure(&f)))),
                    }
                }
                Ok(Some(Frame::Refused(r))) if r.code == Code::Unauthorised => return Err(Refusal::Revoked(r.text)),
                // Frames of an earlier subscription cannot be on a new connection.
                Ok(Some(_)) => {}
                Ok(None) => return Err(Refusal::Other("no reply to subscribe in time".into())),
                Err(ConnError::Malformed(_)) => {}
                Err(e) => return Err(Refusal::Other(e.to_string())),
            }
        }
    }
}

enum Refusal {
    TooOld,
    /// `refused { unauthorised }` after the welcome.
    Revoked(String),
    Other(String),
}

impl Iterator for Feed {
    type Item = Item;

    /// Blocks until the next item: it never ends while the server may come
    /// back, but once it refused the token (`unauthorised`, after a welcome)
    /// it ends.
    fn next(&mut self) -> Option<Item> {
        loop {
            if let Some(item) = self.next_within(Duration::from_secs(1)) {
                return Some(item);
            }
            if self.refused.is_some() {
                return None;
            }
        }
    }
}

impl Drop for Feed {
    fn drop(&mut self) {
        if let Some(c) = self.conn.take() {
            c.shutdown();
        }
    }
}
