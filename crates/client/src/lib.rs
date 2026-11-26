//! `wsp-client`: the one way a process outside the daemon talks to the state
//! core's API (docs/state-core.md section 6), with only what step 1 needs. The
//! CLI (wsp-076) and web-chrome (wsp-078) use it.
//!
//! - [`Client::dial`] reads `api.json` and `api.token`, dials, says `hello`;
//!   nothing answering is [`Silent`], never a panic or a hang.
//! - [`Client::dial_at`] does the same against addresses and a token the
//!   caller holds (another machine's API, its token fetched over ssh).
//! - [`Client::request`] sends one verb and returns the reply with its id.
//! - [`Client::wip`] reads the `wip` verb's cut of the state, typed.
//! - [`Client::apply`] sends a command under one idempotency key, again under
//!   the same key after a lost reply; [`Client::await_order`] follows an order
//!   to a state.
//! - [`Client::throttle`], [`Client::spend`] and [`Client::show`] return the
//!   documents `wsp throttle --json`, `wsp spend --json` and `wsp show <id>
//!   --json` print, the same value (wsp-307).
//! - [`Client::rows`], [`Client::lists`], [`Client::asks`] and
//!   [`Client::readings`] read what web-chrome's census draws (wsp-743): rows
//!   and lists as heads, the asks, and the daemon's readings, each the input a
//!   `wsp-view` projection takes, typed.
//! - [`Client::subscribe`] returns a [`Feed`]: the snapshot and events in
//!   order, each event id once, reconnecting by itself from its last id.
//!
//! A hub is no server of this client's: the shared-work add-on (`wsp-spoke`)
//! has its own client of the hub. It links `wsp-api` and `wsp-store-contract`
//! alone (the server only in tests), so web-chrome links no more of wsp than
//! the protocol.

use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use serde_json::{json, Value};
use wsp_api::{
    Answer, ApiToken, Call, ConnError, Connection, Failure, Frame, IdempotencyKey, NoArgs, Outcome, Reply, ReqId,
    Request, Verb, PROTOCOL,
};
use wsp_store_contract::{Ask, AskKind, AskState, List, ListFilter, Principal, Reading, Refusal, Row, RowFilter, State};

mod dial;
mod orders;
mod subscribe;

pub use dial::Silent;
pub use orders::APPLY_ATTEMPTS;
pub use subscribe::{Feed, Item};

/// How long [`Client::request`] waits for a reply unless [`Client::set_timeout`]
/// says otherwise (or the dial's `wait` is longer).
pub const REQUEST_TIMEOUT: Duration = Duration::from_secs(5);

/// How many rows [`Client::rows`] asks for in one page when its filter sets
/// no `limit` (wsp-377). The server cuts a page shorter where the rows would
/// take its reply past `MAX_LINE`, so this caps the count, not the bytes.
pub const ROWS_PAGE: u32 = 1000;

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

/// Why a view verb ([`Client::throttle`], [`Client::spend`], [`Client::show`])
/// gave no document.
#[derive(Debug, Clone, PartialEq)]
pub enum ViewError {
    /// The request got no reply.
    Request(Error),
    /// The server refused it: `not_found` for a `show` of nothing,
    /// `unsupported` from a server that answers no views (or predates them).
    Refused(Failure),
    /// The reply's `ok` was another verb's answer.
    Answer(Box<Answer>),
}

impl std::fmt::Display for ViewError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ViewError::Request(e) => write!(f, "{e}"),
            ViewError::Refused(r) => write!(f, "refused {:?}: {}", r.code, r.text),
            ViewError::Answer(a) => write!(f, "the server answered another verb: {a:?}"),
        }
    }
}

impl std::error::Error for ViewError {}

/// What it takes to dial again: where the server is, who for, and how long to wait.
#[derive(Debug, Clone)]
pub(crate) struct Dial {
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
    target: Dial,
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
        Client::dial_target(Dial { at: Where::StateDir(state_dir.to_path_buf()), by, wait })
    }

    /// Dial the server at `addrs` (`host:port`, tried in order, as `api.json`
    /// lists them) and say `hello` with `token` as `by`, within `wait` in all:
    /// [`Client::dial`] for a server whose state dir is not on this machine.
    /// No address is [`Silent::BadListenFile`]; a wrong token is
    /// [`Silent::Refused`]. A lost connection, here or in a [`Feed`], dials the
    /// same addresses again.
    pub fn dial_at(addrs: Vec<String>, token: ApiToken, by: Principal, wait: Duration) -> Result<Client, Silent> {
        Client::dial_target(Dial { at: Where::Addrs { addrs, token }, by, wait })
    }

    fn dial_target(mut target: Dial) -> Result<Client, Silent> {
        let wait = target.wait;
        let open = dial::open(&target)?;
        // Every request's `by` must be who the connection is: the server names it in its welcome.
        if let Some(by) = open.welcome.by.clone() {
            target.by = by;
        }
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

    /// Who this connection is: the principal the server's `welcome` named,
    /// else the one the dial said.
    pub fn by(&self) -> &Principal {
        &self.target.by
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

    /// The `whoami` verb (wsp-165): who the server holds this connection as,
    /// and the instance it is on.
    pub fn whoami(&mut self) -> Result<(Principal, String), Error> {
        let reply = self.send_call(Call::Verb(Verb::Whoami(NoArgs {})), None)?;
        match reply.outcome {
            Outcome::Ok(Answer::Whoami { by, server }) => Ok((by, server)),
            Outcome::Ok(other) => Err(Error::Unexpected(format!("whoami was answered {other:?}"))),
            Outcome::Err(f) => Err(orders::refused(f)),
        }
    }

    /// The `wip` verb: open rows whole, every other row and every list
    /// without its bodies, open asks only, seats and claims whole, and the
    /// event id that cut is consistent at. A smaller reply than `state` on a
    /// large store, which is what keeps it under `MAX_LINE`.
    pub fn wip(&mut self) -> Result<(State, wsp_api::EventId), Error> {
        let reply = self.send_call(Call::Verb(Verb::Wip(NoArgs {})), None)?;
        match reply.outcome {
            Outcome::Ok(Answer::Wip { state, at }) => Ok((state, at)),
            Outcome::Ok(other) => Err(Error::Unexpected(format!("wip was answered {other:?}"))),
            Outcome::Err(f) => Err(orders::refused(f)),
        }
    }

    /// The document `wsp throttle --json` prints: this machine's build
    /// throttle and the readings it acts on, as the server's machine reads them.
    pub fn throttle(&mut self) -> Result<Value, ViewError> {
        match self.view("throttle", json!({}))? {
            Answer::Throttle(v) => Ok(v),
            other => Err(ViewError::Answer(Box::new(other))),
        }
    }

    /// The document `wsp spend --json` prints: the last 30 days' tokens and cost.
    pub fn spend(&mut self) -> Result<Value, ViewError> {
        match self.view("spend", json!({}))? {
            Answer::Spend(v) => Ok(v),
            other => Err(ViewError::Answer(Box::new(other))),
        }
    }

    /// The document `wsp show <id> --json` prints, `id` as `wsp show` takes it
    /// (a row's id or prefix, or a piece's id). None is [`ViewError::Refused`]
    /// with `not_found`, and the CLI's sentence as its text.
    pub fn show(&mut self, id: &str) -> Result<Value, ViewError> {
        match self.view("show", json!({ "id": id }))? {
            Answer::Show(v) => Ok(v),
            other => Err(ViewError::Answer(Box::new(other))),
        }
    }

    fn view(&mut self, verb: &str, args: Value) -> Result<Answer, ViewError> {
        match self.request(verb, args).map_err(ViewError::Request)?.outcome {
            Outcome::Ok(a) => Ok(a),
            Outcome::Err(f) => Err(ViewError::Refused(f)),
        }
    }

    /// The `rows` verb: every row `filter` keeps after `filter.after` (all
    /// when unset), in id order, each as its head when `filter.heads` (no
    /// `body` or `prose`). Read in pages (wsp-377) of up to `filter.limit`
    /// rows ([`ROWS_PAGE`] when unset), each from the last id of the one
    /// before, until a page brings no row past it. The server cuts each page
    /// short of `MAX_LINE`, so a full store's rows whole come back where one
    /// reply of them would be `too_large`. A server that does not page
    /// answers every row to each ask; the second brings nothing new, and the
    /// read ends there.
    pub fn rows(&mut self, filter: RowFilter) -> Result<Vec<Row>, Error> {
        let limit = filter.limit.unwrap_or(ROWS_PAGE);
        let mut after = filter.after.clone();
        let mut out = Vec::new();
        loop {
            let ask = RowFilter { after: after.clone(), limit: Some(limit), ..filter.clone() };
            let page = self.read_typed(Verb::Rows(ask), "rows", |a| match a {
                Answer::Rows(rows) => Ok(rows),
                other => Err(other),
            })?;
            let mut fresh: Vec<Row> =
                page.into_iter().filter(|r| after.as_ref().is_none_or(|a| r.id > *a)).collect();
            if fresh.is_empty() {
                break;
            }
            fresh.sort_by(|a, b| a.id.cmp(&b.id));
            after = fresh.last().map(|r| r.id.clone());
            out.extend(fresh);
        }
        Ok(out)
    }

    /// The `lists` verb: every list, each without its text when `filter.heads`.
    pub fn lists(&mut self, filter: ListFilter) -> Result<Vec<List>, Error> {
        self.read_typed(Verb::Lists(filter), "lists", |a| match a {
            Answer::Lists(lists) => Ok(lists),
            other => Err(other),
        })
    }

    /// The `asks` verb: asks, notes and flags of any of `kinds` in any of
    /// `states`, all when either is empty.
    pub fn asks(&mut self, kinds: Vec<AskKind>, states: Vec<AskState>) -> Result<Vec<Ask>, Error> {
        self.read_typed(Verb::Asks { kinds, states }, "asks", |a| match a {
            Answer::Asks(asks) => Ok(asks),
            other => Err(other),
        })
    }

    /// The `readings` verb: each of `names` there is a reading of (every one
    /// when `names` is empty), by name. A reading's `json` is the text of the
    /// `wsp-view` input `wsp_view::reading` names, decoded with
    /// `wsp_view::reading::of`.
    pub fn readings(&mut self, names: &[&str]) -> Result<std::collections::BTreeMap<String, Reading>, Error> {
        let names = names.iter().map(|n| n.to_string()).collect();
        self.read_typed(Verb::Readings { names }, "readings", |a| match a {
            Answer::Readings(readings) => Ok(readings),
            other => Err(other),
        })
    }

    /// One typed read: `verb`'s reply, taken by `want`; another verb's answer
    /// is a malformed reply, and `err` is [`Error::Failed`].
    fn read_typed<T>(&mut self, verb: Verb, name: &str, want: impl FnOnce(Answer) -> Result<T, Answer>) -> Result<T, Error> {
        let reply = self.send_call(Call::Verb(verb), None)?;
        match reply.outcome {
            Outcome::Ok(a) => want(a)
                .map_err(|_| Error::Lost(ConnError::Malformed(format!("`{name}` answered with another verb's answer")))),
            Outcome::Err(f) => Err(Error::Failed(f)),
        }
    }

    fn send(&mut self, verb: &str, args: Value, key: Option<IdempotencyKey>) -> Result<Reply, Error> {
        let call: Call = serde_json::from_value(json!({ "verb": verb, "args": args }))
            .map_err(|e| Error::Args(e.to_string()))?;
        self.send_call(call, key)
    }

    fn send_call(&mut self, call: Call, key: Option<IdempotencyKey>) -> Result<Reply, Error> {
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
