//! One client's connection: the `hello` and its token, then requests, beats
//! and the subscription until the client leaves or misses its beats.
//!
//! Two threads per client: this one reads and answers, and a writer sends
//! whatever is queued for the client (answers, the journal's events, the
//! beat), so a slow client never holds the journal or the beat.

use std::net::TcpStream;
use std::sync::atomic::Ordering;
use std::sync::mpsc::{channel, Sender};
use std::sync::Arc;
use std::time::Instant;

use wsp_host::Tcp;
use wsp_host_api::{
    ConnError, Connection, Frame, Hello, ManagerHello, Message, OrderRefused, OrderFail, Refusal, Refused,
    SeatRefused, SeatsList, WireRange, WspHello, MISSED_BEATS, PROTOCOL,
};

use crate::{Client, Shared, CAPACITY, VERSION};

fn reply(req: Option<wsp_host_api::ReqId>, msg: Message) -> Frame {
    Frame { id: None, req, msg }
}

fn refused(req: Option<wsp_host_api::ReqId>, why: Refusal, detail: &str) -> Frame {
    reply(req, Message::Refused(Refused { why, detail: detail.to_string() }))
}

pub(crate) fn serve(shared: Arc<Shared>, stream: TcpStream) {
    let Ok(mut conn) = Tcp::new(stream) else { return };
    let peer = conn.peer().map_or("?".into(), |p| p.to_string());
    let patience = shared.cfg.beat * MISSED_BEATS;

    let (req, hello) = match conn.recv(patience) {
        Ok(Some(Frame { req, msg: Message::Hello(Hello::Wsp(h)), .. })) => (req, h),
        Ok(Some(f)) => {
            let _ = conn.send(&refused(f.req, Refusal::Malformed, "the first frame must be hello"));
            shared.log.line(&format!("{peer}: closed, its first frame was not a hello"));
            return conn.shutdown();
        }
        Ok(None) => {
            if !shared.stopping.load(Ordering::SeqCst) {
                shared.log.line(&format!("{peer}: closed, no hello within {}s", patience.as_secs()));
            }
            return conn.shutdown();
        }
        Err(ConnError::Malformed(why)) => {
            let _ = conn.send(&refused(None, Refusal::Malformed, &why));
            shared.log.line(&format!("{peer}: closed, its hello was malformed: {why}"));
            return conn.shutdown();
        }
        Err(_) => return conn.shutdown(),
    };
    if !admitted(&shared, &mut conn, &peer, req, &hello) {
        return conn.shutdown();
    }

    let (out, queue) = channel::<Frame>();
    let Ok(mut writer) = conn.try_clone() else { return conn.shutdown() };
    let Ok(closer) = conn.try_clone() else { return conn.shutdown() };
    let id = shared.next_client.fetch_add(1, Ordering::SeqCst);
    {
        let journal = shared.journal.lock().unwrap_or_else(|e| e.into_inner());
        let _ = out.send(reply(req, Message::Hello(Hello::Manager(manager_hello(&shared, journal.head())))));
        let mut clients = shared.clients.lock().unwrap_or_else(|e| e.into_inner());
        if let Some(beat) = shared.last_beat.lock().unwrap_or_else(|e| e.into_inner()).clone() {
            let _ = out.send(beat);
        }
        clients.insert(id, Client { out: out.clone(), subscribed: false, closer });
    }
    std::thread::spawn(move || {
        for frame in queue {
            if writer.send(&frame).is_err() {
                writer.shutdown();
                break;
            }
        }
    });

    let why = follow(&shared, &mut conn, id, &out);
    shared.clients.lock().unwrap_or_else(|e| e.into_inner()).remove(&id);
    conn.shutdown();
    if !shared.stopping.load(Ordering::SeqCst) {
        shared.log.line(&format!("{peer}: {why}"));
    }
}

/// Check the client's `hello`; answer and log a refusal.
fn admitted(shared: &Shared, conn: &mut Tcp, peer: &str, req: Option<wsp_host_api::ReqId>, hello: &WspHello) -> bool {
    if hello.token != shared.token {
        let _ = conn.send(&refused(req, Refusal::Unauthorised, "wrong token"));
        shared.log.line(&format!("{peer}: closed, its hello carried the wrong token ({:?} {})", hello.role, hello.version));
        return false;
    }
    if hello.protocol != PROTOCOL {
        let detail = format!("this manager speaks protocol {PROTOCOL}, the client {}", hello.protocol);
        let _ = conn.send(&refused(req, Refusal::Protocol, &detail));
        shared.log.line(&format!("{peer}: closed, {detail}"));
        return false;
    }
    shared.log.line(&format!(
        "{peer}: hello from {:?} {} (last event {})",
        hello.role,
        hello.version,
        hello.last_event.map_or("none".into(), |e| e.0.to_string())
    ));
    true
}

fn manager_hello(shared: &Shared, head: Option<wsp_host_api::EventId>) -> ManagerHello {
    let wire = WireRange { min: wsp_host::inproc::WIRE_VERSION, max: wsp_host::inproc::WIRE_VERSION };
    ManagerHello {
        host: shared.cfg.host.clone(),
        protocol: PROTOCOL,
        version: VERSION.to_string(),
        compound_sup: wire,
        compound_render: wire,
        journal_head: head,
        seats: vec![],
        capacity: CAPACITY,
    }
}

/// Read and answer the client until it leaves; the reason it left.
fn follow(shared: &Shared, conn: &mut Tcp, id: u64, out: &Sender<Frame>) -> String {
    let patience = shared.cfg.beat * MISSED_BEATS;
    let mut heard = Instant::now();
    loop {
        if shared.stopping.load(Ordering::SeqCst) {
            return "closed, the manager stops".into();
        }
        let left = patience.saturating_sub(heard.elapsed());
        if left.is_zero() {
            return format!("dropped, nothing heard for {MISSED_BEATS} beats");
        }
        match conn.recv(left.min(shared.cfg.beat)) {
            Ok(Some(frame)) => {
                heard = Instant::now();
                answer(shared, id, out, frame);
            }
            Ok(None) => {}
            Err(ConnError::Malformed(why)) => {
                heard = Instant::now();
                let _ = out.send(refused(None, Refusal::Malformed, &why));
            }
            Err(ConnError::Closed) => return "left".into(),
            Err(ConnError::Io(why)) => return format!("closed: {why}"),
        }
    }
}

const NO_SEATS: &str = "this manager runs no seats yet (wsp-022)";

fn answer(shared: &Shared, id: u64, out: &Sender<Frame>, frame: Frame) {
    let req = frame.req;
    let send = |msg: Message| {
        let _ = out.send(reply(req, msg));
    };
    match frame.msg {
        Message::Beat(_) => {}
        Message::Subscribe(s) => subscribe(shared, id, out, s.from),
        Message::Seats(_) => send(Message::SeatsList(SeatsList { seats: vec![] })),
        Message::Hello(_) => {
            let _ = out.send(refused(req, Refusal::Malformed, "hello was already said"));
        }
        Message::SeatSpawn(s) => send(Message::SeatRefused(SeatRefused {
            key: s.key,
            seat: s.seat,
            why: Refusal::Unsupported,
            detail: NO_SEATS.into(),
        })),
        Message::OrderDeliver(o) => send(Message::OrderRefused(OrderRefused { order: o.order, seat: o.seat, why: OrderFail::NoSeat })),
        Message::OrderCancel(o) => send(Message::OrderRefused(OrderRefused { order: o.order, seat: o.seat, why: OrderFail::NoSeat })),
        Message::SeatStop(_) | Message::SeatRead(_) => {
            let _ = out.send(refused(req, Refusal::NoSeat, NO_SEATS));
        }
        Message::JobRun(_) | Message::JobStop(_) => {
            let _ = out.send(refused(req, Refusal::Unsupported, "this manager runs no jobs yet (M3)"));
        }
        Message::TreeCheckout(_) | Message::TreeLand(_) | Message::TreeCopy(_) => {
            let _ = out.send(refused(req, Refusal::Unsupported, "tree verbs wait for Q6 (T1 holds)"));
        }
        Message::Refused(r) => shared.log.line(&format!("client {id} refused something: {:?} {}", r.why, r.detail)),
        // What the manager sends, never what it is sent.
        Message::SeatStarted(_)
        | Message::SeatRefused(_)
        | Message::SeatStopped(_)
        | Message::OrderDelivered(_)
        | Message::OrderRefused(_)
        | Message::OrderAcked(_)
        | Message::Turn(_)
        | Message::SeatSaid(_)
        | Message::SeatSpent(_)
        | Message::SeatDid(_)
        | Message::SeatStuck(_)
        | Message::SeatsList(_)
        | Message::SeatScreen(_)
        | Message::JobStarted(_)
        | Message::JobEnded(_) => {
            let _ = out.send(refused(req, Refusal::Malformed, "a manager's message, not one wsp sends"));
        }
    }
}

/// Queue every journalled event after `from`, then follow live. The journal
/// is held across both, so no event is sent twice or falls between.
fn subscribe(shared: &Shared, id: u64, out: &Sender<Frame>, from: Option<wsp_host_api::EventId>) {
    let journal = shared.journal.lock().unwrap_or_else(|e| e.into_inner());
    match journal.after(from) {
        Ok((frames, skipped)) => {
            if skipped > 0 {
                shared.log.line(&format!("client {id}: {skipped} journal lines this build cannot read were not sent"));
            }
            for f in frames {
                let _ = out.send(f);
            }
        }
        Err(e) => {
            shared.log.line(&format!("client {id}: the journal could not be read: {e}"));
            let _ = out.send(refused(None, Refusal::Failed, &format!("the journal could not be read: {e}")));
            return;
        }
    }
    if let Some(c) = shared.clients.lock().unwrap_or_else(|e| e.into_inner()).get_mut(&id) {
        c.subscribed = true;
    }
}
