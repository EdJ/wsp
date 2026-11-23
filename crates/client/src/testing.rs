//! An in-memory reference hub for the sync verbs (wsp-459), behind
//! `test-support`: for this crate's tests, and for anyone's until the hub's own
//! store and server land (wsp-460).
//!
//! [`MemHub`] is the verbs' meaning, with no IO: [`MemHub::answer`] answers one
//! [`SyncCall`] against its objects and refs. [`FakeHub`] serves one over TCP
//! on loopback, so a real [`Client`] dials it: the hello needs the hub's token
//! (any `by` is taken), every sync verb is answered from the shared
//! [`MemHub`], and every other verb is `unsupported`.
//!
//! No membership: any connection reads and writes any workspace. That is the
//! hub's (wsp-460), not the protocol's.

use std::collections::BTreeMap;
use std::io::{BufRead, BufReader, Write};
use std::net::{Shutdown, TcpListener, TcpStream};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, MutexGuard};
use std::time::Duration;

use wsp_api::{
    Answer, ApiToken, Blob, Call, Code, EventId, Failure, Frame, Move, ObjectId, Outcome, RefMove, Refs, Refused,
    Reply, SyncCall, Welcome, WorkspaceId, PROTOCOL,
};
use wsp_store_contract::{Principal, Subject};

use crate::Client;

/// The sync verbs' meaning over objects and refs in memory.
#[derive(Debug, Default)]
pub struct MemHub {
    objects: BTreeMap<ObjectId, Vec<u8>>,
    refs: BTreeMap<WorkspaceId, Refs>,
    writes: u64,
    moves: u64,
}

impl MemHub {
    pub fn new() -> MemHub {
        MemHub::default()
    }

    /// One sync verb's outcome.
    pub fn answer(&mut self, call: SyncCall) -> Outcome {
        match call {
            SyncCall::Heads { workspace } => Outcome::Ok(Answer::Heads(self.refs(&workspace))),
            SyncCall::Have { ids } => {
                Outcome::Ok(Answer::Missing(ids.into_iter().filter(|id| !self.objects.contains_key(id)).collect()))
            }
            SyncCall::Put { objects } => match self.put(objects) {
                Ok(stored) => Outcome::Ok(Answer::Stored { stored }),
                Err(text) => Outcome::Err(Failure { code: Code::Invalid, text, refusal: None }),
            },
            SyncCall::Get { ids } => Outcome::Ok(Answer::Objects(
                ids.into_iter()
                    .filter_map(|id| self.objects.get(&id).map(|bytes| Blob { id, bytes: bytes.clone() }))
                    .collect(),
            )),
            SyncCall::Move(mv) => Outcome::Ok(Answer::Move(self.move_ref(mv))),
        }
    }

    /// Every object is held to its id before any is kept; one already held is
    /// not written again.
    fn put(&mut self, objects: Vec<Blob>) -> Result<u32, String> {
        for blob in &objects {
            wsp_objects::verify(&blob.id, &blob.bytes).map_err(|e| format!("{}: {e}", blob.id))?;
        }
        let mut stored = 0;
        for blob in objects {
            if let std::collections::btree_map::Entry::Vacant(slot) = self.objects.entry(blob.id) {
                slot.insert(blob.bytes);
                stored += 1;
            }
        }
        self.writes += stored as u64;
        Ok(stored)
    }

    fn move_ref(&mut self, mv: RefMove) -> Move {
        self.moves += 1;
        let refs = self.refs.entry(mv.workspace.clone()).or_insert_with(|| Refs::new(mv.workspace));
        let head = refs.get(&mv.subject).copied();
        if head != mv.from {
            return Move::Stale { head };
        }
        refs.set(mv.subject, mv.to);
        Move::Moved { head: mv.to }
    }

    /// `workspace`'s refs; none for one it has never seen.
    pub fn refs(&self, workspace: &WorkspaceId) -> Refs {
        self.refs.get(workspace).cloned().unwrap_or_else(|| Refs::new(workspace.clone()))
    }

    /// Moves a head outside the protocol, as another writer's move would.
    pub fn set_head(&mut self, workspace: &WorkspaceId, subject: Subject, head: ObjectId) {
        self.refs.entry(workspace.clone()).or_insert_with(|| Refs::new(workspace.clone())).set(subject, head);
    }

    /// Keeps an object outside the protocol, unchecked.
    pub fn insert(&mut self, id: ObjectId, bytes: Vec<u8>) {
        self.objects.insert(id, bytes);
    }

    pub fn holds(&self, id: &ObjectId) -> bool {
        self.objects.contains_key(id)
    }

    /// How many objects it holds.
    pub fn len(&self) -> usize {
        self.objects.len()
    }

    pub fn is_empty(&self) -> bool {
        self.objects.is_empty()
    }

    /// How many objects `sync.put` has written, in all.
    pub fn writes(&self) -> u64 {
        self.writes
    }

    /// How many `refs.move`s it has answered, moved or stale.
    pub fn moves(&self) -> u64 {
        self.moves
    }
}

/// A [`MemHub`] served on loopback. Dropping it stops it.
pub struct FakeHub {
    addr: String,
    token: ApiToken,
    hub: Arc<Mutex<MemHub>>,
    stop: Arc<AtomicBool>,
}

/// The token every [`FakeHub`] takes.
pub const FAKE_HUB_TOKEN: &str = "fake-hub-token";

impl FakeHub {
    /// Bind `127.0.0.1:0` and serve an empty hub.
    pub fn start() -> FakeHub {
        let listener = TcpListener::bind("127.0.0.1:0").expect("bind loopback");
        let addr = listener.local_addr().expect("a bound address").to_string();
        let token = ApiToken(FAKE_HUB_TOKEN.into());
        let hub = Arc::new(Mutex::new(MemHub::new()));
        let stop = Arc::new(AtomicBool::new(false));
        let (h, s, t) = (hub.clone(), stop.clone(), token.clone());
        std::thread::spawn(move || {
            for stream in listener.incoming() {
                if s.load(Ordering::SeqCst) {
                    break;
                }
                let Ok(stream) = stream else { continue };
                let (h, t) = (h.clone(), t.clone());
                std::thread::spawn(move || serve(stream, &t, &h));
            }
        });
        FakeHub { addr, token, hub, stop }
    }

    /// Where it listens, `127.0.0.1:<port>`.
    pub fn addr(&self) -> String {
        self.addr.clone()
    }

    pub fn token(&self) -> ApiToken {
        self.token.clone()
    }

    /// The hub's state, locked.
    pub fn hub(&self) -> MutexGuard<'_, MemHub> {
        self.hub.lock().unwrap_or_else(|e| e.into_inner())
    }

    /// The hub's state, shared, for a test that acts on it from elsewhere.
    pub fn shared(&self) -> Arc<Mutex<MemHub>> {
        self.hub.clone()
    }

    /// A client dialled to it as `by`.
    pub fn client(&self, by: Principal) -> Client {
        Client::dial_at(vec![self.addr()], self.token(), by, Duration::from_secs(5)).expect("the fake hub answers")
    }
}

impl Drop for FakeHub {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::SeqCst);
        // Wake the accept loop so it sees the stop.
        let _ = TcpStream::connect(&self.addr);
    }
}

fn write(stream: &mut TcpStream, frame: &Frame) -> bool {
    let mut line = serde_json::to_vec(frame).expect("a frame is JSON");
    line.push(b'\n');
    stream.write_all(&line).is_ok()
}

fn serve(stream: TcpStream, token: &ApiToken, hub: &Mutex<MemHub>) {
    let Ok(mut out) = stream.try_clone() else { return };
    let mut lines = BufReader::new(stream).lines();
    match lines.next().and_then(Result::ok).map(|l| serde_json::from_str::<Frame>(&l)) {
        Some(Ok(Frame::Hello(hello))) if hello.token == *token => {
            let welcome = Welcome { server: "fake-hub".into(), protocol: PROTOCOL, head: EventId(0) };
            if !write(&mut out, &Frame::Welcome(welcome)) {
                return;
            }
        }
        _ => {
            let refused = Refused { code: Code::Unauthorised, text: "not the fake hub's token".into() };
            write(&mut out, &Frame::Refused(refused));
            let _ = out.shutdown(Shutdown::Both);
            return;
        }
    }
    for line in lines {
        let Ok(line) = line else { return };
        let Ok(Frame::Request(req)) = serde_json::from_str::<Frame>(&line) else { continue };
        let outcome = match req.call {
            Call::Sync(call) => hub.lock().unwrap_or_else(|e| e.into_inner()).answer(call),
            _ => Outcome::Err(Failure {
                code: Code::Unsupported,
                text: "the fake hub answers the sync verbs alone".into(),
                refusal: None,
            }),
        };
        if !write(&mut out, &Frame::Reply(Reply { id: req.id, outcome, events: Vec::new() })) {
            return;
        }
    }
}
