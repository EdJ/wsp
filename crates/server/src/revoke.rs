//! Revocation drops what is open (wsp-162). The hub refuses a revoked token at
//! the next hello (wsp-161); this is the other half: every welcomed connection
//! is registered with the credential its hello resolved to ([`Entry`]), and a
//! thread that follows the store's head ([`follow`], through `wait_for`, as
//! `subscribe` does) closes each connection that holds a credential a
//! `hub.token.revoked` event names, or belongs to a user a `hub.user.revoked`
//! event names.
//!
//! A cut connection's loop ends with "dropped, its token was revoked" in
//! `api.log`, and a subscriber's feed gets a last `refused { unauthorised }`
//! frame before the close, so its client stops redialling with that token.
//!
//! A connection can be welcomed between the revocation and the thread's
//! reading of it. [`Clients`] holds what the thread has seen revoked beside the
//! open connections, under one lock: [`Clients::add`] refuses a connection
//! that is already revoked, and [`Clients::revoke_token`] cuts one that is
//! open, so whichever comes second finds the other.

use std::collections::{BTreeMap, BTreeSet};
use std::net::{Shutdown, TcpStream};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::Duration;

use wsp_api::{Code, Connection, Frame, Refused, MAX_EVENTS};
use wsp_store_contract::{Event, EventKind, Principal, Subject};

use crate::conn::Writer;
use crate::Shared;

/// What a connection that was cut for a revoked token says in `api.log`.
pub(crate) const DROPPED: &str = "dropped, its token was revoked";

/// What the refused frame tells a subscriber.
const TEXT: &str = "the token is revoked";

/// One welcomed connection.
pub(crate) struct Entry {
    /// A handle that closes it.
    pub(crate) stream: TcpStream,
    pub(crate) writer: Writer,
    /// The credential its hello resolved to, where the gate has them.
    pub(crate) credential: Option<String>,
    pub(crate) by: Principal,
    /// Set once `subscribe` was answered: its feed is told before the close.
    pub(crate) subscribed: Arc<AtomicBool>,
    /// Set when it was cut for a revocation.
    pub(crate) revoked: Arc<AtomicBool>,
}

/// The welcomed connections, by number, and what has been revoked since the
/// server started.
#[derive(Default)]
pub(crate) struct Clients {
    pub(crate) open: BTreeMap<u64, Entry>,
    tokens: BTreeSet<String>,
    users: BTreeSet<String>,
}

impl Clients {
    fn hit(&self, e: &Entry) -> bool {
        e.credential.as_ref().is_some_and(|c| self.tokens.contains(c))
            || matches!(&e.by, Principal::Person(u) if self.users.contains(u))
    }

    /// Register `entry` as connection `n`; handed back when its credential was
    /// revoked already (it is not registered).
    pub(crate) fn add(&mut self, n: u64, entry: Entry) -> Result<(), Entry> {
        if self.hit(&entry) {
            return Err(entry);
        }
        self.open.insert(n, entry);
        Ok(())
    }

    /// Cut every open connection holding credential `id`, and any later one.
    pub(crate) fn revoke_token(&mut self, id: &str) {
        self.tokens.insert(id.to_string());
        self.cut(|e| e.credential.as_deref() == Some(id));
    }

    /// Cut every open connection of user `name`, and any later one.
    pub(crate) fn revoke_user(&mut self, name: &str) {
        self.users.insert(name.to_string());
        self.cut(|e| matches!(&e.by, Principal::Person(u) if u == name));
    }

    fn cut(&mut self, which: impl Fn(&Entry) -> bool) {
        for e in self.open.values().filter(|e| which(e)) {
            cut(e);
        }
    }
}

/// Close `e`, a subscriber's feed told first. The loop that serves it sees the
/// close and logs [`DROPPED`].
fn cut(e: &Entry) {
    if e.revoked.swap(true, Ordering::SeqCst) {
        return;
    }
    {
        // The frame and the close under one hold of the writer, so that no
        // beat or event of the feed comes between them.
        let mut w = e.writer.lock().unwrap_or_else(|p| p.into_inner());
        if e.subscribed.load(Ordering::SeqCst) {
            let _ = w.send(&Frame::Refused(Refused { code: Code::Unauthorised, text: TEXT.into() }));
        }
        w.shutdown();
    }
    let _ = e.stream.shutdown(Shutdown::Both);
}

/// Follow the store's head from where it is now, and cut what each revocation
/// names, until the server stops. Run on a thread of its own.
pub(crate) fn follow(shared: Arc<Shared>) {
    const POLL: Duration = Duration::from_millis(100);
    let mut cursor = loop {
        match shared.store.head() {
            Ok(h) => break h,
            Err(_) if shared.stopping() => return,
            Err(_) => std::thread::sleep(POLL),
        }
    };
    while !shared.stopping() {
        let moved = match shared.store.wait_for(cursor, POLL) {
            Ok(head) => head > cursor,
            Err(_) => {
                std::thread::sleep(POLL);
                continue;
            }
        };
        if !moved {
            continue;
        }
        match shared.store.events_after(cursor, MAX_EVENTS as usize) {
            Ok(events) if events.is_empty() => std::thread::sleep(Duration::from_millis(5)),
            Ok(events) => {
                for e in events {
                    cursor = e.id;
                    seen(&shared, &e);
                }
            }
            Err(_) => std::thread::sleep(POLL),
        }
    }
}

fn seen(shared: &Shared, e: &Event) {
    let mut clients = shared.clients.lock().unwrap_or_else(|p| p.into_inner());
    match (&e.kind, &e.subject) {
        (EventKind::HubTokenRevoked, Subject::Credential(id)) => clients.revoke_token(&id.0),
        (EventKind::HubUserRevoked, Subject::User(name)) => clients.revoke_user(&name.0),
        _ => {}
    }
}
