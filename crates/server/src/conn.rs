//! One connection (docs/state-core.md 4.1, wsp-api's module docs): `hello`
//! first, answered `welcome`, or `refused` and closed; then requests, each
//! answered by one reply with its `id`.
//!
//! - A read verb is one call on the store's `Reads` or `Events` (`lists`,
//!   `asks`, `machines`, and `orders` of no seat, which `Reads` has no verb
//!   for, read the one `snapshot`).
//! - `subscribe` is answered `subscribed`, or `too_old`; then the
//!   `subscribe` module's thread writes its feed beside the replies.
//! - Every command verb is `unsupported` at step 1. A verb this build does not
//!   know is `unsupported`; a known verb whose `args` did not fit, `malformed`.
//! - A line that does not decode is answered `malformed` and the connection
//!   stays up: as a reply to its `id` when it has one, else as `refused`.
//! - A request whose `by` is not the hello's is `forbidden`; one whose `v` is
//!   not [`PROTOCOL`], `protocol`.
//! - A client silent for [`MISSED_BEATS`] beats is dropped. Any frame counts,
//!   a malformed line included; a subscriber with nothing to ask keeps its
//!   connection with a cheap read (`events { from: <its last>, limit: 0 }`).

use std::net::TcpStream;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;
use std::time::Instant;

use serde_json::Value;
use wsp_api::{
    Answer, Call, Code, ConnError, Connection, Failure, Frame, Hello, Outcome, Refused, Reply, ReqId, Request, Verb,
    Welcome, COMMAND_VERBS, MAX_EVENTS, MISSED_BEATS, PROTOCOL, READ_VERBS,
};
use wsp_store_contract::{Error, Refusal};

use crate::tcp::Tcp;
use crate::{subscribe, Shared};

/// The writing half of a connection, shared by the replies and the feed.
pub(crate) type Writer = Arc<Mutex<Tcp>>;

/// Send `frame` on `w`; `false` when the connection is gone.
pub(crate) fn send(w: &Writer, frame: &Frame) -> bool {
    w.lock().unwrap_or_else(|e| e.into_inner()).send(frame).is_ok()
}

/// Whether two tokens are equal, comparing every byte whatever the first
/// difference, so the time taken does not say how much of a guess was right.
fn same(a: &str, b: &str) -> bool {
    a.len() == b.len() && a.bytes().zip(b.bytes()).fold(0u8, |acc, (x, y)| acc | (x ^ y)) == 0
}

fn refuse(conn: &mut Tcp, code: Code, text: &str) {
    let _ = conn.send(&Frame::Refused(Refused { code, text: text.to_string() }));
    conn.shutdown();
}

/// Serve one accepted connection until it ends.
pub(crate) fn serve(shared: Arc<Shared>, stream: TcpStream, n: u64) {
    let Ok(mut conn) = Tcp::new(stream) else { return };
    let peer = conn.peer().map_or("?".into(), |p| p.to_string());
    let patience = shared.cfg.beat * MISSED_BEATS;

    let hello: Hello = match conn.recv_line(patience) {
        Ok(Some(line)) => match serde_json::from_slice::<Frame>(&line) {
            Ok(Frame::Hello(h)) => h,
            Ok(_) => {
                shared.log.line(&format!("{peer}: closed, its first frame was not a hello"));
                return refuse(&mut conn, Code::Malformed, "the first frame must be hello");
            }
            Err(e) => {
                shared.log.line(&format!("{peer}: closed, its first line was not a frame"));
                return refuse(&mut conn, Code::Malformed, &format!("the first frame must be hello: {e}"));
            }
        },
        Ok(None) => {
            if !shared.stopping() {
                shared.log.line(&format!("{peer}: closed, no hello within {}s", patience.as_secs_f32()));
            }
            return conn.shutdown();
        }
        Err(_) => return conn.shutdown(),
    };
    if !same(&hello.token.0, &shared.token.0) {
        shared.log.line(&format!("{peer}: refused, a wrong token ({})", hello.client));
        return refuse(&mut conn, Code::Unauthorised, "the token is not this instance's");
    }
    if hello.protocol != PROTOCOL {
        return refuse(
            &mut conn,
            Code::Protocol,
            &format!("this server speaks protocol {PROTOCOL}, not {}", hello.protocol),
        );
    }
    let head = match shared.store.head() {
        Ok(h) => h,
        Err(e) => return refuse(&mut conn, Code::Backend, &format!("the store did not answer: {e:?}")),
    };
    let welcome = Welcome { server: shared.cfg.instance.clone(), protocol: PROTOCOL, head };
    if conn.send(&Frame::Welcome(welcome)).is_err() {
        return conn.shutdown();
    }

    let (Ok(writer), Ok(handle)) = (conn.try_clone(), conn.stream_clone()) else {
        return conn.shutdown();
    };
    shared.clients.lock().unwrap_or_else(|e| e.into_inner()).insert(n, handle);
    let mut c = Client {
        shared: &shared,
        hello: &hello,
        writer: Arc::new(Mutex::new(writer)),
        closed: Arc::new(AtomicBool::new(false)),
        feed: None,
    };
    let why = if shared.stopping() { "closed, the server stops".to_string() } else { c.follow(&mut conn) };
    c.closed.store(true, Ordering::SeqCst);
    conn.shutdown();
    shared.clients.lock().unwrap_or_else(|e| e.into_inner()).remove(&n);
    if let Some(feed) = c.feed.take() {
        let _ = feed.join();
    }
    if !why.is_empty() {
        shared.log.line(&format!("{peer} ({}): {why}", hello.client));
    }
}

/// A welcomed connection.
struct Client<'a> {
    shared: &'a Arc<Shared>,
    hello: &'a Hello,
    writer: crate::conn::Writer,
    /// Set by either half when the connection is done.
    closed: Arc<AtomicBool>,
    /// The subscription's thread, once `subscribe` was answered.
    feed: Option<JoinHandle<()>>,
}

impl Client<'_> {
    /// Read and answer until the client leaves; why it ended (empty when the
    /// client closed it).
    fn follow(&mut self, conn: &mut Tcp) -> String {
        let beat = self.shared.cfg.beat;
        let patience = beat * MISSED_BEATS;
        let mut heard = Instant::now();
        loop {
            if self.shared.stopping() {
                return "closed, the server stops".into();
            }
            if self.closed.load(Ordering::SeqCst) {
                return "closed, its feed could not be written".into();
            }
            let left = patience.saturating_sub(heard.elapsed());
            if left.is_zero() {
                return format!("dropped, nothing heard for {MISSED_BEATS} beats");
            }
            match conn.recv_line(left.min(beat)) {
                Ok(Some(line)) => {
                    heard = Instant::now();
                    if !self.answer_line(&line) {
                        return "closed, a reply could not be written".into();
                    }
                }
                Ok(None) => {}
                Err(ConnError::Closed) => return String::new(),
                Err(e) => return format!("closed: {e}"),
            }
        }
    }

    /// Answer one line; `false` when the reply could not be written.
    fn answer_line(&mut self, line: &[u8]) -> bool {
        match serde_json::from_slice::<Frame>(line) {
            Ok(Frame::Request(req)) => self.answer(req),
            Ok(_) => send(
                &self.writer,
                &Frame::Refused(Refused { code: Code::Malformed, text: "after hello, only requests".into() }),
            ),
            Err(e) => {
                let text = format!("not a frame: {e}");
                let id = serde_json::from_slice::<Value>(line)
                    .ok()
                    .and_then(|v| v.get("id").and_then(Value::as_u64))
                    .map(ReqId);
                match id {
                    Some(id) => send(&self.writer, &reply(id, Err(failure(Code::Malformed, text)))),
                    None => send(&self.writer, &Frame::Refused(Refused { code: Code::Malformed, text })),
                }
            }
        }
    }

    fn answer(&mut self, req: Request) -> bool {
        let id = req.id;
        if req.v != PROTOCOL {
            let text = format!("this server speaks v {PROTOCOL}, not {}", req.v);
            return send(&self.writer, &reply(id, Err(failure(Code::Protocol, text))));
        }
        if req.by != self.hello.by {
            let text = format!(
                "this connection is {}, not {}",
                wsp_api::principal::text(&self.hello.by),
                wsp_api::principal::text(&req.by)
            );
            let f = Failure { code: Code::Forbidden, text, refusal: Some(Refusal::Forbidden { by: req.by }) };
            return send(&self.writer, &reply(id, Err(f)));
        }
        let outcome = match req.call {
            Call::Verb(Verb::Subscribe(sub)) => return self.subscribe(id, sub),
            Call::Verb(verb) => read(self.shared, verb),
            Call::Command(cmd) => {
                let verb = serde_json::to_value(&cmd)
                    .ok()
                    .and_then(|v| v.get("verb").and_then(Value::as_str).map(str::to_string))
                    .unwrap_or_default();
                Err(failure(Code::Unsupported, format!("{verb} is a command, and step 1's server runs none")))
            }
            Call::Unknown { verb, .. } => {
                if READ_VERBS.contains(&verb.as_str()) || COMMAND_VERBS.contains(&verb.as_str()) {
                    Err(failure(Code::Malformed, format!("{verb}'s args do not fit it")))
                } else {
                    Err(failure(Code::Unsupported, format!("{verb} is no verb this server knows")))
                }
            }
        };
        send(&self.writer, &reply(id, outcome))
    }

    fn subscribe(&mut self, id: ReqId, sub: wsp_api::Subscribe) -> bool {
        if self.feed.is_some() {
            let f = failure(Code::Invalid, "this connection is subscribed already; it holds one subscription".into());
            return send(&self.writer, &reply(id, Err(f)));
        }
        let opened = match subscribe::open(self.shared, &sub) {
            Ok(o) => o,
            Err(f) => return send(&self.writer, &reply(id, Err(f))),
        };
        if !send(&self.writer, &reply(id, Ok(Answer::Subscribed))) {
            return false;
        }
        let (shared, writer, closed) = (self.shared.clone(), self.writer.clone(), self.closed.clone());
        self.feed = Some(std::thread::spawn(move || subscribe::follow(shared, writer, closed, opened)));
        true
    }
}

fn reply(id: ReqId, outcome: Result<Answer, Failure>) -> Frame {
    let outcome = match outcome {
        Ok(a) => Outcome::Ok(a),
        Err(f) => Outcome::Err(f),
    };
    Frame::Reply(Reply { id, outcome, events: vec![] })
}

fn failure(code: Code, text: String) -> Failure {
    Failure { code, text, refusal: None }
}

/// A refusal's code, as the API spells it.
fn code_of(r: &Refusal) -> Code {
    match r {
        Refusal::Stale { .. } => Code::Stale,
        Refusal::TooOld { .. } => Code::TooOld,
        Refusal::Conflict { .. } => Code::Conflict,
        Refusal::Held { .. } => Code::Held,
        Refusal::NotFound { .. } => Code::NotFound,
        Refusal::Illegal { .. } => Code::Illegal,
        Refusal::Forbidden { .. } => Code::Forbidden,
        Refusal::Invalid { .. } => Code::Invalid,
        Refusal::Unknown => Code::Unknown,
    }
}

/// A store error as a reply's `err`: a refusal whole, or `backend`.
pub(crate) fn store_failure(e: Error) -> Failure {
    match e {
        Error::Refused(r) => Failure { code: code_of(&r), text: format!("{r:?}"), refusal: Some(r) },
        Error::Backend { reason } => failure(Code::Backend, reason),
    }
}

/// One read verb, as one call on the store.
fn read(shared: &Shared, verb: Verb) -> Result<Answer, Failure> {
    let s = &shared.store;
    let r = match verb {
        Verb::State(_) => s.snapshot().map(|(state, at)| Answer::State { state, at }),
        Verb::Row { row } => s.row(&row).map(Answer::Row),
        Verb::Rows(filter) => s.rows(&filter).map(Answer::Rows),
        Verb::List { list } => s.list(&list).map(Answer::List),
        Verb::Lists(_) => s.snapshot().map(|(st, _)| Answer::Lists(st.lists.into_values().collect())),
        Verb::Seat { seat } => s.seat(&seat).map(Answer::Seat),
        Verb::Seats(filter) => s.seats(&filter).map(Answer::Seats),
        Verb::Orders { seat: Some(seat), states } => s.orders_for(&seat, &states).map(Answer::Orders),
        Verb::Orders { seat: None, states } => s.snapshot().map(|(st, _)| {
            Answer::Orders(
                st.orders.into_values().filter(|o| states.is_empty() || states.contains(&o.state)).collect(),
            )
        }),
        Verb::Asks { kinds, states } => s.snapshot().map(|(st, _)| {
            Answer::Asks(
                st.asks
                    .into_values()
                    .filter(|a| kinds.is_empty() || kinds.contains(&a.kind))
                    .filter(|a| states.is_empty() || states.contains(&a.state))
                    .collect(),
            )
        }),
        Verb::Machines(_) => s.snapshot().map(|(st, _)| Answer::Machines(st.machines.into_values().collect())),
        Verb::Events { from, limit } => {
            let limit = limit.unwrap_or(MAX_EVENTS).min(MAX_EVENTS) as usize;
            s.events_after(from, limit).and_then(|events| Ok(Answer::Events { events, head: s.head()? }))
        }
        Verb::Subscribe(_) => unreachable!("subscribe is answered by its own path"),
    };
    r.map_err(store_failure)
}
