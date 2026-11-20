//! `wsp-client`: the one way a process outside the daemon talks to the state
//! core's API (docs/state-core.md section 6), with only what step 1 needs. The
//! CLI (wsp-076) and web-chrome (wsp-078) use it.
//!
//! - [`Client::dial`] reads `api.json` and `api.token`, dials, says `hello`;
//!   nothing answering is [`Silent`], never a panic or a hang.
//! - [`Client::dial_at`] does the same against addresses and a token the
//!   caller holds (another machine's API, its token fetched over ssh).
//! - [`Client::request`] sends one verb and returns the reply with its id.
//! - [`Client::apply`] sends a command under one idempotency key, again under
//!   the same key after a lost reply; [`Client::await_order`] follows an order
//!   to a state.
//! - [`Client::subscribe`] returns a [`Feed`]: the snapshot and events in
//!   order, each event id once, reconnecting by itself from its last id.
//!
//! It links `wsp-api` and `wsp-store-contract` alone (the server only in
//! tests), so web-chrome links no more of wsp than the protocol.

use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use serde_json::{json, Value};
use wsp_api::{ApiToken, Call, ConnError, Connection, Failure, Frame, IdempotencyKey, Reply, ReqId, Request, PROTOCOL};
use wsp_store_contract::{Principal, Refusal};

mod dial;
mod orders;
mod subscribe;

pub use dial::Silent;
pub use orders::APPLY_ATTEMPTS;
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
    /// The server refused a command, and said why; the same command under the
    /// same key would be refused the same, so it is not sent again.
    Refused(Refusal),
    /// The server answered `err` with a code and no refusal (a verb it does not
    /// run, a malformed request, a failed store).
    Failed(Failure),
    /// The server answered with something other than what the verb answers.
    Unexpected(String),
}

impl std::fmt::Display for Error {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Error::Timeout => f.write_str("no reply in time"),
            Error::Lost(e) => write!(f, "{e}"),
            Error::Silent(s) => write!(f, "{s}"),
            Error::Args(why) => write!(f, "the arguments do not fit the verb: {why}"),
            Error::Refused(r) => write!(f, "the server refused it: {r:?}"),
            Error::Failed(x) => write!(f, "the server failed it: {:?}: {}", x.code, x.text),
            Error::Unexpected(what) => write!(f, "the server's answer was not the verb's: {what}"),
        }
    }
}

impl std::error::Error for Error {}

/// What it takes to dial again: where the server is, who for, and how long to wait.
#[derive(Debug, Clone)]
pub(crate) struct Target {
    pub(crate) at: Where,
    pub(crate) by: Principal,
    pub(crate) wait: Duration,
}

/// Where the server is: named by a state dir's `api.json` and `api.token`
/// (read again on each dial, so a restarted server's new port is found), or
/// given outright.
#[derive(Debug, Clone)]
pub(crate) enum Where {
    StateDir(PathBuf),
    Addrs { addrs: Vec<String>, token: ApiToken },
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
        Client::dial_target(Target { at: Where::StateDir(state_dir.to_path_buf()), by, wait })
    }

    /// Dial the server at `addrs` (`host:port`, tried in order, as `api.json`
    /// lists them) and say `hello` with `token` as `by`, within `wait` in all:
    /// [`Client::dial`] for a server whose state dir is not on this machine.
    /// No address is [`Silent::BadListenFile`]; a wrong token is
    /// [`Silent::Refused`]. A lost connection, here or in a [`Feed`], dials the
    /// same addresses again.
    pub fn dial_at(addrs: Vec<String>, token: ApiToken, by: Principal, wait: Duration) -> Result<Client, Silent> {
        Client::dial_target(Target { at: Where::Addrs { addrs, token }, by, wait })
    }

    fn dial_target(target: Target) -> Result<Client, Silent> {
        let wait = target.wait;
        let open = dial::open(&target)?;
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
            let open = dial::open(&self.target).map_err(Error::Silent)?;
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
