//! What step 2's senders need of the client, so that no caller re-implements it
//! (docs/state-core.md 4.5): [`Client::apply`], a command under one idempotency
//! key however many times it has to be sent, and [`Client::await_order`], an
//! order followed to the state the caller waits for.

use std::collections::hash_map::RandomState;
use std::hash::{BuildHasher, Hasher};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use serde_json::{json, Value};
use wsp_api::{Answer, Outcome, Reply};
use wsp_store_contract::{Applied, Command, EventId, EventKind, IdempotencyKey, Order, OrderId, OrderState, Subject};

use crate::{dial, Client, Error, Item};

/// How many times [`Client::apply`] sends a command, at most. The one timeout is
/// shared out among them, so a reply that never comes costs the timeout and not
/// that many times it.
pub const APPLY_ATTEMPTS: u32 = 3;

/// The pause between two sends when the server could not be dialled in between.
const REDIAL_PAUSE: Duration = Duration::from_millis(25);

/// A key no other call has minted: the clock, the process, a count and what the
/// standard library's per-process randomness makes of them. Not a uuid (no
/// dependency for one), but as unique as a retry needs it to be.
fn mint_key() -> IdempotencyKey {
    static COUNT: AtomicU64 = AtomicU64::new(0);
    let nanos = SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_nanos()).unwrap_or_default();
    let mut h = RandomState::new().build_hasher();
    h.write_u128(nanos);
    h.write_u32(std::process::id());
    h.write_u64(COUNT.fetch_add(1, Ordering::Relaxed));
    IdempotencyKey(format!("{nanos:x}-{:x}-{:016x}", std::process::id(), h.finish()))
}

/// `cmd` as the verb and the `args` of its request.
fn split(cmd: &Command) -> Result<(String, Value), Error> {
    let mut v = serde_json::to_value(cmd).map_err(|e| Error::Args(e.to_string()))?;
    let verb = v.get("verb").and_then(Value::as_str).map(str::to_string);
    let verb = verb.ok_or_else(|| Error::Args("a command with no verb".into()))?;
    Ok((verb, v.get_mut("args").map(Value::take).unwrap_or(Value::Null)))
}

/// The state an `order.*` event leaves its order in. `order.refused` leaves it
/// in `queued` or `failed` by the attempts, which the event does not say.
fn lands_in(kind: EventKind) -> Option<OrderState> {
    Some(match kind {
        EventKind::OrderQueued => OrderState::Queued,
        EventKind::OrderTyped => OrderState::Typed,
        EventKind::OrderDelivered => OrderState::Delivered,
        EventKind::OrderAcknowledged => OrderState::Acknowledged,
        EventKind::OrderDone => OrderState::Done,
        EventKind::OrderCancelled => OrderState::Cancelled,
        EventKind::OrderFailed => OrderState::Failed,
        _ => return None,
    })
}

impl Client {
    /// Apply `cmd`: send it under a key minted for this call, and after a lost
    /// reply or a lost connection, send it again under the same key, so that a
    /// command the server did apply is not applied twice and the retry gets the
    /// first result (2.2, 4.5). All of it is within the one timeout
    /// ([`Client::set_timeout`]), at most [`APPLY_ATTEMPTS`] sends.
    ///
    /// A refusal is the server's [`Refusal`](wsp_store_contract::Refusal), as
    /// [`Error::Refused`], and is never retried: the same command under the
    /// same key would be refused the same. Nor is any other `err` reply
    /// ([`Error::Failed`]) or a server that cannot be dialled before anything
    /// was sent ([`Error::Silent`]).
    pub fn apply(&mut self, cmd: &Command) -> Result<Applied, Error> {
        let (verb, args) = split(cmd)?;
        let key = mint_key();
        let timeout = self.timeout;
        let deadline = Instant::now() + timeout;
        let mut sent = false;
        let mut last = Error::Timeout;
        let mut attempt = 0;
        let outcome = loop {
            let left = deadline.saturating_duration_since(Instant::now());
            if attempt >= APPLY_ATTEMPTS || (attempt > 0 && left.is_zero()) {
                break Err(last);
            }
            // Each send waits an even share of what is left.
            self.timeout = (left / (APPLY_ATTEMPTS - attempt)).max(Duration::from_millis(1));
            attempt += 1;
            match self.request_keyed(&verb, args.clone(), key.clone()) {
                Ok(reply) => break answer(reply),
                Err(e @ (Error::Timeout | Error::Lost(_))) => {
                    sent = true;
                    last = e;
                }
                // Nothing went out on a connection that could not be made: the
                // first time, that is the answer; after a send, the server may be
                // coming back, and the key makes the next send safe.
                Err(Error::Silent(s)) if sent => {
                    last = Error::Silent(s);
                    std::thread::sleep(REDIAL_PAUSE.min(deadline.saturating_duration_since(Instant::now())));
                }
                Err(e) => break Err(e),
            }
        };
        self.timeout = timeout;
        outcome
    }

    /// The order `id`, once it is in one of `settled`; `None` when `wait` runs
    /// out first. The order's own state is read once (`orders`), so one already
    /// settled answers at once. Otherwise `order.*` events on that id are followed
    /// (`subscribe { from }`, from the head as it stood before the read) until
    /// one lands it in `settled`. The returned order is the one read, in the state
    /// the event landed it in; an `order.refused` is followed by a read, as it
    /// does not say whether the order went back to `queued` or to `failed`.
    ///
    /// An order nothing has is `None` too, after `wait`: it may yet be sent. A
    /// server that cannot be reached for the dial's `wait` is
    /// [`Error::Silent`], never a hang.
    pub fn await_order(&mut self, id: &OrderId, settled: &[OrderState], wait: Duration) -> Result<Option<Order>, Error> {
        let deadline = Instant::now() + wait;
        // The head first: events written between it and the read are seen twice
        // (once in the read, once in the feed) and not missed.
        let from = match self.read_head() {
            Ok(head) => Some(head),
            Err(Error::Refused(_)) | Err(Error::Failed(_)) => None,
            Err(e) => return Err(e),
        };
        let mut held = self.read_order(id)?;
        if let Some(o) = held.as_ref().filter(|o| settled.contains(&o.state)) {
            return Ok(Some(o.clone()));
        }
        let mut feed = self.subscribe(from);
        let bound = self.target.wait;
        let mut down_since: Option<Instant> = None;
        loop {
            let left = deadline.saturating_duration_since(Instant::now());
            if left.is_zero() {
                return Ok(None);
            }
            let item = feed.next_within(left.min(Duration::from_millis(100)));
            // Not connected: for how long, and is it the server or this call?
            if feed.why_silent().is_some() {
                let since = *down_since.get_or_insert_with(Instant::now);
                if since.elapsed() >= bound {
                    match dial::open(&self.target) {
                        Err(s) => return Err(Error::Silent(s)),
                        Ok(open) => {
                            open.conn.shutdown();
                            down_since = None;
                        }
                    }
                }
            } else {
                down_since = None;
            }
            match item {
                Some(Item::State { state, .. } | Item::Resync { state, .. }) => {
                    held = state.orders.get(id).cloned();
                }
                Some(Item::Event(e)) if e.subject == Subject::Order(id.clone()) => {
                    if e.kind == EventKind::OrderRefused {
                        held = self.read_order(id)?;
                    } else if let (Some(now), Some(o)) = (lands_in(e.kind), held.as_mut()) {
                        o.state = now;
                    } else if lands_in(e.kind).is_some() {
                        // An order the read did not find has appeared: read it.
                        held = self.read_order(id)?;
                    }
                }
                _ => continue,
            }
            if let Some(o) = held.as_ref().filter(|o| settled.contains(&o.state)) {
                return Ok(Some(o.clone()));
            }
        }
    }

    /// The server's head: `events` after what is known of it, no more than none.
    fn read_head(&mut self) -> Result<EventId, Error> {
        let args = json!({ "from": self.head.0, "limit": 0 });
        match self.read("events", args)? {
            Answer::Events { head, .. } => Ok(head),
            other => Err(Error::Unexpected(format!("events was answered {other:?}"))),
        }
    }

    /// Order `id`, as the server holds it now.
    fn read_order(&mut self, id: &OrderId) -> Result<Option<Order>, Error> {
        match self.read("orders", json!({}))? {
            Answer::Orders(orders) => Ok(orders.into_iter().find(|o| &o.id == id)),
            other => Err(Error::Unexpected(format!("orders was answered {other:?}"))),
        }
    }

    /// A read, once more if the connection it was on had been lost.
    fn read(&mut self, verb: &str, args: Value) -> Result<Answer, Error> {
        let reply = match self.request(verb, args.clone()) {
            Err(Error::Lost(_)) => self.request(verb, args)?,
            other => other?,
        };
        match reply.outcome {
            Outcome::Ok(a) => Ok(a),
            Outcome::Err(f) => Err(refused(f)),
        }
    }
}

/// A reply to a command: its events, or why not.
fn answer(reply: Reply) -> Result<Applied, Error> {
    match reply.outcome {
        Outcome::Ok(Answer::Applied) => Ok(Applied { events: reply.events }),
        Outcome::Ok(other) => Err(Error::Unexpected(format!("a command was answered {other:?}"))),
        Outcome::Err(f) => Err(refused(f)),
    }
}

/// An `err` reply as the caller's error: the server's refusal where it gave
/// one, else the failure whole.
pub(crate) fn refused(f: wsp_api::Failure) -> Error {
    match f.refusal {
        Some(r) => Error::Refused(r),
        None => Error::Failed(f),
    }
}
