//! One connection (docs/state-core.md 4.1, wsp-api's module docs): `hello`
//! first, answered `welcome`, or `refused` and closed; then requests, each
//! answered by one reply with its `id`.
//!
//! - A read verb is one call on the store's `Reads` or `Events` (`lists`,
//!   `asks`, `machines`, and `orders` of no seat, which `Reads` has no verb
//!   for, read the one `snapshot`). `wip` is answered from the server's cut
//!   cached by event head (the `wip` module), cut again once the head moves.
//!   An `events` page stops at [`MAX_EVENTS_BYTES`] as well as at `limit`,
//!   and its `through` says where (wsp-378).
//! - `throttle`, `spend` and `show` are the CLI's documents, from the
//!   [`crate::Views`] the server was started with (wsp-307), `unsupported`
//!   without one.
//! - `subscribe` is answered `subscribed`, or `too_old`; then the
//!   `subscribe` module's thread writes its feed beside the replies.
//! - A command [`Orders::owns`] is applied through the store's `apply`, under
//!   the request's key and the connection's principal, and answered `applied`
//!   with the events it wrote (none for a repeat of a key: the first result).
//!   Who may send it is checked first ([`may_command`]): `order.send` and
//!   `order.cancel` a person or a seat; `order.deliver` and the executor's
//!   answers (`order.delivered`, `order.acked`, `order.refused`,
//!   `order.done`) only the instance itself (`wsp:<instance>`) or a manager,
//!   because delivery and the fold apply them in process. Anyone else is
//!   refused `unauthorised`; a command with no key, `invalid`. Whether the
//!   machine decides them is the store's domain's business
//!   ([`crate::domain::Mode`]): under `Mirrored` they come back `illegal`,
//!   state `mirrored`.
//! - Every other command verb (`mirror` included: the daemon applies it in
//!   process) is `unsupported`. A verb this build does not know is
//!   `unsupported`; a known verb whose `args` did not fit, `malformed`.
//! - A line that does not decode is answered `malformed` and the connection
//!   stays up: as a reply to its `id` when it has one, else as `refused`.
//! - A request whose `by` is not the connection's is `forbidden`; one whose
//!   `v` is not [`PROTOCOL`], `protocol`. The connection is the hello's `by`,
//!   behind the instance's token, and `welcome` names it.
//! - A reply whose line would be longer than [`MAX_LINE`] is not sent: the
//!   client would drop the connection on it (wsp-377). The request is
//!   answered `too_large` instead, naming the length. `state` of a full store
//!   is such a reply; `rows` of one is too, unless asked a page (`after` or
//!   `limit`): the rows the filter keeps, in id order, cut short where the
//!   next would take the reply past the line.
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
    Welcome, COMMAND_VERBS, MAX_EVENTS, MAX_EVENTS_BYTES, MAX_LINE, MISSED_BEATS, PROTOCOL, READ_VERBS,
};
use wsp_machines::Orders;
use wsp_store_contract::{Command, Error, IdempotencyKey, Principal, Refusal, Row, RowFilter};

use crate::tcp::Tcp;
use crate::{subscribe, Shared};

/// The writing half of a connection, shared by the replies and the feed.
pub(crate) type Writer = Arc<Mutex<Tcp>>;

/// Send `frame` on `w`; `false` when the connection is gone.
pub(crate) fn send(w: &Writer, frame: &Frame) -> bool {
    w.lock().unwrap_or_else(|e| e.into_inner()).send(frame).is_ok()
}

/// Write `line` (a whole frame, newline ended) on `w`; `false` when the connection is gone.
fn send_bytes(w: &Writer, line: &[u8]) -> bool {
    w.lock().unwrap_or_else(|e| e.into_inner()).write_line(line).is_ok()
}

/// Send the reply `frame` to `id`, or `too_large` in its place where its
/// line would be longer than [`MAX_LINE`] (wsp-377).
fn send_reply(w: &Writer, id: ReqId, frame: &Frame) -> bool {
    match serde_json::to_vec(frame) {
        Ok(mut line) => {
            line.push(b'\n');
            send_sized(w, id, &line)
        }
        Err(_) => send(w, frame),
    }
}

/// Write the reply `line` to `id` as it is, or `too_large` in its place where
/// it is longer than [`MAX_LINE`]: a client stops reading at that length and
/// drops the connection, so the request would get no answer at all.
fn send_sized(w: &Writer, id: ReqId, line: &[u8]) -> bool {
    if line.len() > MAX_LINE {
        return send(w, &reply(id, Err(too_large(line.len()))));
    }
    send_bytes(w, line)
}

/// The `too_large` for a reply of `len` bytes.
fn too_large(len: usize) -> Failure {
    let text = format!(
        "the reply is {len} bytes, past the {MAX_LINE} a line may be: ask for less \
         (`rows` pages with `after` and `limit`, and takes `heads`)"
    );
    failure(Code::TooLarge, text)
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
    let by = hello.by.clone();
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
    let welcome = Welcome { server: shared.cfg.instance.clone(), protocol: PROTOCOL, head, by: Some(by.clone()) };
    if conn.send(&Frame::Welcome(welcome)).is_err() {
        return conn.shutdown();
    }

    let (Ok(writer), Ok(handle)) = (conn.try_clone(), conn.stream_clone()) else {
        return conn.shutdown();
    };
    let writer: Writer = Arc::new(Mutex::new(writer));
    shared.clients.lock().unwrap_or_else(|e| e.into_inner()).insert(n, handle);
    let mut c = Client { shared: &shared, by, writer, closed: Arc::new(AtomicBool::new(false)), feed: None };
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
    /// Who the connection is: the hello's `by`.
    by: Principal,
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
                    // A client waiting on its reply is not silent: the time taken to
                    // answer (a page of rows near the line, wsp-377) is not its to count.
                    heard = Instant::now();
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
        if req.by != self.by {
            let text = format!(
                "this connection is {}, not {}",
                wsp_api::principal::text(&self.by),
                wsp_api::principal::text(&req.by)
            );
            let f = Failure { code: Code::Forbidden, text, refusal: Some(Refusal::Forbidden { by: req.by }) };
            return send(&self.writer, &reply(id, Err(f)));
        }
        let outcome = match req.call {
            Call::Verb(Verb::Subscribe(sub)) => return self.subscribe(id, sub),
            // Who this connection is: the principal it was welcomed as, which `read` does not hold (wsp-165).
            Call::Verb(Verb::Whoami(_)) => Ok(Answer::Whoami { by: self.by.clone(), server: self.shared.cfg.instance.clone() }),
            // From the cut cached by head, spliced into the reply rather than serialised again (wsp-373).
            Call::Verb(Verb::Wip(_)) => match self.shared.wip.answer(self.shared.store.as_ref()) {
                Ok(ok) => return send_sized(&self.writer, id, &crate::wip::line(id, &ok)),
                Err(e) => Err(store_failure(e)),
            },
            Call::Verb(verb) => read(self.shared, verb),
            Call::Command(cmd) => return self.command(id, req.key, cmd),
            Call::Unknown { verb, .. } => {
                if READ_VERBS.contains(&verb.as_str()) || COMMAND_VERBS.contains(&verb.as_str()) {
                    Err(failure(Code::Malformed, format!("{verb}'s args do not fit it")))
                } else {
                    Err(failure(Code::Unsupported, format!("{verb} is no verb this server knows")))
                }
            }
        };
        send_reply(&self.writer, id, &reply(id, outcome))
    }

    /// One command: the orders machine's, from a principal that may send it,
    /// with a key, applied; every other `unsupported`.
    fn command(&mut self, id: ReqId, key: Option<IdempotencyKey>, cmd: Command) -> bool {
        let verb = verb_of(&cmd);
        if !Orders::owns(&cmd) {
            let text = format!("{verb} is a command this server does not run over the API");
            return send(&self.writer, &reply(id, Err(failure(Code::Unsupported, text))));
        }
        let by = &self.by;
        if !may_command(&self.shared.cfg.instance, by, &cmd) {
            let text = if matches!(cmd, Command::OrderSend { .. } | Command::OrderCancel { .. }) {
                format!("{verb} is sent by a person or a seat, not {}", wsp_api::principal::text(by))
            } else {
                format!(
                    "{verb} is applied in process: only wsp:{} or a manager sends it, not {}",
                    self.shared.cfg.instance,
                    wsp_api::principal::text(by)
                )
            };
            return send(&self.writer, &reply(id, Err(failure(Code::Unauthorised, text))));
        }
        let Some(key) = key else {
            let text = format!("{verb} needs an idempotency key (the request's `key`)");
            return send(&self.writer, &reply(id, Err(failure(Code::Invalid, text))));
        };
        let frame = match self.shared.store.apply(cmd, key, by.clone()) {
            Ok(applied) => Frame::Reply(Reply { id, outcome: Outcome::Ok(Answer::Applied), events: applied.events }),
            Err(e) => reply(id, Err(store_failure(e))),
        };
        send(&self.writer, &frame)
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

/// The verb a command is, as the wire names it (`order.send`).
fn verb_of(cmd: &Command) -> String {
    serde_json::to_value(cmd)
        .ok()
        .and_then(|v| v.get("verb").and_then(Value::as_str).map(str::to_string))
        .unwrap_or_default()
}

/// Whether `by` may send `cmd` over the API, on the instance `instance`:
/// `order.send` and `order.cancel` a person or a seat; the orders machine's
/// other verbs (`order.deliver` and the executor's answers) the instance
/// itself or a manager. Any other command, nobody.
pub(crate) fn may_command(instance: &str, by: &Principal, cmd: &Command) -> bool {
    match cmd {
        Command::OrderSend { .. } | Command::OrderCancel { .. } => {
            matches!(by, Principal::Person(_) | Principal::Seat(_))
        }
        c if Orders::owns(c) => match by {
            Principal::Wsp(i) => i == instance,
            Principal::Manager(_) => true,
            _ => false,
        },
        _ => false,
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

/// Cut an `events` page to [`MAX_EVENTS_BYTES`] of compact JSON: it stops
/// before the event that would pass it, but keeps its first, so a reader
/// paging by `through` always moves on.
fn cut_by_bytes(events: &mut Vec<wsp_store_contract::Event>) {
    let mut bytes = 0usize;
    let keep = events
        .iter()
        .position(|e| {
            let mut n = Count(0);
            let _ = serde_json::to_writer(&mut n, e);
            bytes += n.0 + 1; // its comma
            bytes > MAX_EVENTS_BYTES
        })
        .map_or(events.len(), |i| i.max(1));
    events.truncate(keep);
}

/// A writer that only counts what is written to it.
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

/// One read verb, as one call on the store.
fn read(shared: &Shared, verb: Verb) -> Result<Answer, Failure> {
    let s = &shared.store;
    let r = match verb {
        Verb::State(_) => s.snapshot().map(|(state, at)| Answer::State { state, at }),
        // `answer` serves it from the cache; this uncached cut is for any other caller.
        Verb::Wip(_) => s.wip_snapshot().map(|(state, at)| Answer::Wip { state, at }),
        Verb::Row { row } => s.row(&row).map(Answer::Row),
        Verb::Rows(filter) if filter.after.is_some() || filter.limit.is_some() => {
            s.rows(&filter).map(|rows| Answer::Rows(page(rows, &filter)))
        }
        Verb::Rows(filter) => s.rows(&filter).map(Answer::Rows),
        Verb::List { list } => s.list(&list).map(Answer::List),
        Verb::Lists(filter) => s.lists(&filter).map(Answer::Lists),
        Verb::Readings { names } => s.readings(&names).map(Answer::Readings),
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
            s.events_after(from, limit).and_then(|mut events| {
                cut_by_bytes(&mut events);
                let through = events.last().map_or(from, |e| e.id);
                Ok(Answer::Events { events, head: s.head()?, through })
            })
        }
        Verb::Subscribe(_) => unreachable!("subscribe is answered by its own path"),
        Verb::Throttle(_) | Verb::Spend(_) | Verb::Show { .. } => return view(shared, verb),
        Verb::Whoami(_) => return Err(failure(Code::Backend, "whoami is answered by the connection".into())),
    };
    r.map_err(store_failure)
}

/// What a page of rows leaves of [`MAX_LINE`] for the reply around them
/// (`{"type":"reply","id":…,"ok":{"rows":[…]},"events":[]}` and its newline).
const PAGE_SLACK: usize = 4096;

/// One page of `rows` (wsp-377): those after `filter.after`, in id order, at
/// most `filter.limit` of them, and no more than fit one reply under
/// [`MAX_LINE`]. A page holds at least one row where any is left, so a client
/// that asks from its page's last id always moves on; a row too long for a
/// line alone is answered `too_large`.
fn page(mut rows: Vec<Row>, filter: &RowFilter) -> Vec<Row> {
    rows.sort_by(|a, b| a.id.cmp(&b.id));
    let limit = filter.limit.map_or(usize::MAX, |n| n as usize);
    let mut room = MAX_LINE - PAGE_SLACK;
    let mut out = Vec::new();
    for row in rows.into_iter().filter(|r| filter.after.as_ref().is_none_or(|a| r.id > *a)) {
        if out.len() >= limit {
            break;
        }
        // Its JSON and the comma before the next.
        let size = serde_json::to_vec(&row).map_or(0, |b| b.len()) + 1;
        if !out.is_empty() && size > room {
            break;
        }
        room = room.saturating_sub(size);
        out.push(row);
    }
    out
}

/// A view verb, answered by the [`crate::Views`] the server was started with,
/// whole; `unsupported` without one.
fn view(shared: &Shared, verb: Verb) -> Result<Answer, Failure> {
    let Some(views) = &shared.cfg.views else {
        return Err(failure(Code::Unsupported, "this server was started with nothing that answers the view verbs".into()));
    };
    match verb {
        Verb::Throttle(_) => views.throttle().map(Answer::Throttle),
        Verb::Spend(_) => views.spend().map(Answer::Spend),
        Verb::Show { id } => views.show(&id).map(Answer::Show),
        _ => unreachable!("only the view verbs come here"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use wsp_store_contract::{At, MachineName, OrderId, SeatId};

    #[test]
    fn who_may_send_the_orders_machines_commands() {
        let o = OrderId("o-1".into());
        let send = Command::OrderSend { to: SeatId("cpd-1".into()), text: "go".into(), first: false };
        let cancel = Command::OrderCancel { order: o.clone() };
        let acked = Command::OrderAcked { order: o.clone(), turn_began: At(1) };
        let done = Command::OrderDone { order: o.clone() };
        let ed = Principal::Person("ed".into());
        let seat = Principal::Seat(SeatId("cpd-1".into()));
        let me = Principal::Wsp("wsp".into());
        let other = Principal::Wsp("elsewhere".into());
        let manager = Principal::Manager(MachineName("mac".into()));
        let executor = Principal::Executor(MachineName("mac".into()));
        for by in [&ed, &seat] {
            assert!(may_command("wsp", by, &send) && may_command("wsp", by, &cancel), "{by:?} sends and cancels");
            assert!(!may_command("wsp", by, &acked) && !may_command("wsp", by, &done), "{by:?} answers nothing");
        }
        for by in [&me, &manager] {
            assert!(may_command("wsp", by, &acked) && may_command("wsp", by, &done), "{by:?} answers");
        }
        for by in [&other, &executor] {
            assert!(!may_command("wsp", by, &acked), "{by:?} is neither this instance nor a manager");
        }
        let mirror = Command::Mirror { put: vec![], gone: vec![] };
        assert!(!may_command("wsp", &me, &mirror), "mirror is no API command");
    }
}
