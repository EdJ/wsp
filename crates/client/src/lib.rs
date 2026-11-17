//! `wsp-client`: the one way a process outside the daemon talks to the state
//! core's API (docs/state-core.md section 6), with only what step 1 needs. The
//! CLI (wsp-076) and web-chrome (wsp-078) use it.
//!
//! - [`Client::dial`] reads `api.json` and `api.token`, dials, says `hello`;
//!   nothing answering is [`Silent`], never a panic or a hang.
//! - [`Client::request`] sends one verb and returns the reply with its id.
//! - [`Client::subscribe`] returns a [`Feed`]: the snapshot and events in
//!   order, each event id once, reconnecting by itself from its last id.
//!
//! It links `wsp-api` and `wsp-store-contract` alone (the server only in
//! tests), so web-chrome links no more of wsp than the protocol.

use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use serde_json::{json, Value};
use wsp_api::{Call, ConnError, Connection, Frame, IdempotencyKey, Reply, ReqId, Request, PROTOCOL};
use wsp_store_contract::Principal;

mod dial;
mod subscribe;

pub use dial::Silent;
pub use subscribe::{Feed, Item};

/// How long [`Client::request`] waits for a reply unless [`Client::set_timeout`]
/// says otherwise (or the dial's `wait` is longer).
pub const REQUEST_TIMEOUT: Duration = Duration::from_secs(5);

/// Why a request got no reply.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Error {
    /// No reply within the timeout; the connection was dropped (a late reply
    /// would be taken for the next one's).
    Timeout,
    /// The connection failed; the next request dials again.
    Lost(ConnError),
    /// No server could be dialled for a request that had to dial again.
    Silent(Silent),
    /// `args` do not fit the verb, so the request could not be made.
    Args(String),
}

impl std::fmt::Display for Error {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Error::Timeout => f.write_str("no reply in time"),
            Error::Lost(e) => write!(f, "{e}"),
            Error::Silent(s) => write!(f, "{s}"),
            Error::Args(why) => write!(f, "the arguments do not fit the verb: {why}"),
        }
    }
}

impl std::error::Error for Error {}

/// What it takes to dial again: the state dir, who for, and how long to wait.
#[derive(Debug, Clone)]
pub(crate) struct Target {
    pub(crate) state_dir: PathBuf,
    pub(crate) by: Principal,
    pub(crate) wait: Duration,
}

/// A welcomed connection to the server, for requests.
pub struct Client {
    target: Target,
    conn: Option<dial::Tcp>,
    next: u64,
    timeout: Duration,
    server: String,
    head: wsp_api::EventId,
}

impl Client {
    /// Dial the server `state_dir` names (its `api.json` and `api.token`) and
    /// say `hello` as `by`, within `wait` in all. [`Silent`] says why not.
    pub fn dial(state_dir: &Path, by: Principal, wait: Duration) -> Result<Client, Silent> {
        let target = Target { state_dir: state_dir.to_path_buf(), by, wait };
        let open = dial::open(&target.state_dir, &target.by, wait)?;
        Ok(Client {
            target,
            conn: Some(open.conn),
            next: 1,
            timeout: REQUEST_TIMEOUT.max(wait),
            server: open.welcome.server,
            head: open.welcome.head,
        })
    }

    /// The instance the server named in its `welcome` (the first one's).
    pub fn server(&self) -> &str {
        &self.server
    }

    /// The head the server's `welcome` named when this client dialled.
    pub fn head(&self) -> wsp_api::EventId {
        self.head
    }

    /// The one timeout of a request, from sending it to its reply.
    pub fn set_timeout(&mut self, timeout: Duration) {
        self.timeout = timeout;
    }

    /// Send `verb` with `args` (`{}` or `null` for a verb with none) and return
    /// the reply with the same id. A refusal is a [`Reply`] too: its outcome
    /// is `err`. After a lost connection the next call dials again.
    pub fn request(&mut self, verb: &str, args: Value) -> Result<Reply, Error> {
        self.send(verb, args, None)
    }

    /// [`Client::request`] for a command: it carries the idempotency `key`, and
    /// a retry with the same key gets the first result (2.2).
    pub fn request_keyed(&mut self, verb: &str, args: Value, key: IdempotencyKey) -> Result<Reply, Error> {
        self.send(verb, args, Some(key))
    }

    fn send(&mut self, verb: &str, args: Value, key: Option<IdempotencyKey>) -> Result<Reply, Error> {
        let call: Call = serde_json::from_value(json!({ "verb": verb, "args": args }))
            .map_err(|e| Error::Args(e.to_string()))?;
        let id = ReqId(self.next);
        self.next += 1;
        let request = Request { v: PROTOCOL, id, key, by: self.target.by.clone(), call };
        let deadline = Instant::now() + self.timeout;
        if self.conn.is_none() {
            let open = dial::open(&self.target.state_dir, &self.target.by, self.target.wait).map_err(Error::Silent)?;
            self.conn = Some(open.conn);
        }
        let conn = self.conn.as_mut().expect("dialled above");
        let outcome = (|| {
            conn.send(&Frame::Request(request)).map_err(Error::Lost)?;
            loop {
                let left = deadline.saturating_duration_since(Instant::now());
                match conn.recv(left) {
                    Ok(Some(Frame::Reply(r))) if r.id == id => return Ok(r),
                    Ok(Some(_)) => {}
                    Ok(None) => return Err(Error::Timeout),
                    Err(ConnError::Malformed(_)) => {}
                    Err(e) => return Err(Error::Lost(e)),
                }
            }
        })();
        if outcome.is_err() {
            if let Some(c) = self.conn.take() {
                c.shutdown();
            }
        }
        outcome
    }

    /// A feed of the state from `from`: the events after that id, or, with
    /// `None`, the snapshot and then every event after it. It dials on its own
    /// connection when first read, and again whenever that one is lost.
    pub fn subscribe(&self, from: Option<wsp_api::EventId>) -> Feed {
        Feed::new(self.target.clone(), from, self.timeout)
    }
}
