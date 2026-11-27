//! The spoke's own client of the hub, speaking `wsp-hub-protocol` and nothing
//! else: the spoke links neither the local API's types (`wsp-api`) nor its
//! client (`wsp-client`) for it. Moved here from `wsp-client`, where the hub's
//! client code sat beside the local server's; the bytes on the wire are the
//! ones that code sent.
//!
//! - [`Client::dial_hub`] dials a hub with a [`Credential`] (the user's token,
//!   from `$WSP_HUB_TOKEN` or `~/.config/wsp/hubs.json`, [`login`]) and says
//!   `hello`; over TLS for a `tls://` URL ([`tls`]), a private CA pinned with
//!   [`Client::dial_hub_ca`]. Nothing answering is [`Silent`], never a panic
//!   or a hang.
//! - [`Client::request`] sends one verb and returns the reply with its id;
//!   [`Client::whoami`] and [`Client::state`] are the reads a spoke makes,
//!   typed; [`Client::apply`] sends an identity command under one idempotency
//!   key, again under the same key after a lost reply.
//! - [`Client::subscribe`] returns a [`Feed`]: the snapshot and events in
//!   order, each event id once, reconnecting by itself from its last id.
//! - [`Client::sync`] syncs one workspace with the hub by hash, and the sync,
//!   id, lease and message verbs are there one by one ([`sync`]).
//! - [`login_github`] logs in with GitHub: the hub runs the device flow and
//!   hands back a user and a token.
//! - [`hub_cert_expiry`] reads when the hub's certificate runs out.
//! - [`token`] mints a hub token and its hash, for `wsp hub user add`.
//! - [`words`] says a hub's refusal in a sentence, never Rust's debug form.

use std::time::{Duration, Instant};

use wsp_hub_protocol::{
    Answer, ApiToken, Call, ConnError, Connection, EventId, Failure, Frame, IdempotencyKey, IdentityCommand, NoArgs,
    Outcome, Principal, Refusal, Reply, ReqId, Request, State, Verb, PROTOCOL,
};

mod dial;
pub(crate) mod login;
pub mod machine;
mod subscribe;
mod sync;
mod tls;
pub mod token;
pub mod words;

pub use dial::{hub_cert_expiry, login_github, login_github_on, Silent};
pub use login::{
    forget_welcome, hub_ca, hub_ca_in, hub_home, hub_login_in, hub_url, hub_url_in, keep_welcome, kept_admin, kept_server, Credential, HubLogin,
    HUBS_FILE, HUB_CA_ENV, HUB_HOME_ENV, HUB_TOKEN_ENV, HUB_URL_ENV, HUB_USER_ENV, TOKEN_ENV, WELCOME_FILE,
};
pub use subscribe::{Feed, Item};
pub use sync::{ObjectSource, Stale, SyncError, SyncReport, IDS_PER_REQUEST, PUT_BYTES};

/// How long [`Client::request`] waits for a reply unless [`Client::set_timeout`]
/// says otherwise (or the dial's `wait` is longer).
pub const REQUEST_TIMEOUT: Duration = Duration::from_secs(5);

/// How many times [`Client::apply`] sends a command, at most. The one timeout is
/// shared out among them, so a reply that never comes costs the timeout and not
/// that many times it.
pub const APPLY_ATTEMPTS: u32 = 3;

/// The pause between two sends when the hub could not be dialled in between.
const REDIAL_PAUSE: Duration = Duration::from_millis(25);

/// Why a request got no reply.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Error {
    /// No reply within the timeout; the connection was dropped (a late reply
    /// would be taken for the next one's).
    Timeout,
    /// The connection failed; the next request dials again.
    Lost(ConnError),
    /// The hub could not be dialled for a request that had to dial again.
    Silent(Silent),
    /// The hub refused a command, and said why; the same command under the
    /// same key would be refused the same, so it is not sent again.
    /// The second field is the hub's text of it (empty where it gave none):
    /// what only the words carry, as the machine a lease is held on.
    Refused(Refusal, String),
    /// The hub answered `err` with a code and no refusal (a verb it does not
    /// run, a malformed request, a failed store).
    Failed(Failure),
    /// The hub answered with something other than what the verb answers.
    Unexpected(String),
}

impl std::fmt::Display for Error {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Error::Timeout => f.write_str("no reply in time"),
            Error::Lost(e) => write!(f, "{e}"),
            Error::Silent(s) => write!(f, "{s}"),
            Error::Refused(r, text) => write!(f, "the server refused it: {}", words::refusal(r, text)),
            Error::Failed(x) => write!(f, "the server failed it: {}", words::failure(x)),
            Error::Unexpected(what) => write!(f, "the server's answer was not the verb's: {what}"),
        }
    }
}

impl std::error::Error for Error {}

/// An `err` reply as the caller's error: the hub's refusal where it gave one,
/// else the failure whole.
pub(crate) fn refused(f: Failure) -> Error {
    match f.refusal {
        Some(r) => Error::Refused(r, f.text),
        None => Error::Failed(f),
    }
}

/// What it takes to dial again: the hub's `host:port`, the user's token, TLS
/// when the URL is `tls://`, who the hello says it is for, and how long to wait.
#[derive(Debug, Clone)]
pub(crate) struct Dial {
    pub(crate) addr: String,
    pub(crate) token: ApiToken,
    pub(crate) tls: Option<tls::Tls>,
    pub(crate) by: Principal,
    pub(crate) wait: Duration,
    /// The machine every hello names ([`machine`]); none where its id could be
    /// neither read nor made, and the hub then holds leases by user alone.
    pub(crate) machine: Option<wsp_hub_protocol::Machine>,
}

/// A welcomed connection to the hub, for requests.
pub struct Client {
    target: Dial,
    conn: Option<dial::Tcp>,
    next: u64,
    timeout: Duration,
    server: String,
    head: EventId,
    /// The machine the hub's first `welcome` said it holds this connection's
    /// leases on (none from a hub that keys leases by user alone).
    held_on: Option<wsp_hub_protocol::Machine>,
    /// Whether the hub's first `welcome` said this connection's user is a hub
    /// admin; none from a hub that does not say.
    admin: Option<bool>,
}

impl Client {
    /// Dial the hub at `url` and say `hello` with the user's token from
    /// `credential`, within `wait` in all. `tls://host:port` is TLS, the
    /// hub's certificate checked against the platform's roots and for `host`;
    /// `tcp://host:port` and a bare `host:port` are plain TCP, for loopback
    /// and a private network. A refused certificate is [`Silent::Tls`],
    /// naming its fault. The hub ignores the hello's `by`, so the client sends
    /// the user's name (`$WSP_HUB_USER`, else `$USER`) as a hint only, and
    /// makes every request as the principal the `welcome` names. A lost
    /// connection dials the same hub again.
    pub fn dial_hub(url: &str, credential: Credential, wait: Duration) -> Result<Client, Silent> {
        Client::dial_hub_ca(url, credential, None, wait)
    }

    /// [`Client::dial_hub`] trusting the CA in `ca` (a PEM file) and nothing
    /// else, for a hub whose certificate a private CA signed (a test, a
    /// self-hosted hub): `--ca <file>`, [`hub_ca`]. It applies to `tls://` only.
    /// The hello names this machine ([`machine`]) as kept under the hub home
    /// ([`hub_home`]), whatever home the credential was read from: in a
    /// process the hub home is the one machine, so every dial of it (a test's
    /// spokes too, unless each sets `$WSP_HUB_HOME`) holds leases alike.
    pub fn dial_hub_ca(url: &str, credential: Credential, ca: Option<&std::path::Path>, wait: Duration) -> Result<Client, Silent> {
        let machine = login::hub_home().and_then(|home| machine::this(&home).ok());
        Client::dial_hub_on(url, credential, ca, wait, machine)
    }

    fn dial_hub_on(
        url: &str,
        credential: Credential,
        ca: Option<&std::path::Path>,
        wait: Duration,
        machine: Option<wsp_hub_protocol::Machine>,
    ) -> Result<Client, Silent> {
        let Credential::User(token) = credential;
        let (addr, secure) = login::hub_addr(url)?;
        let tls = if secure { Some(tls::Tls::new(&addr, ca)?) } else { None };
        let mut target = Dial { addr, token: ApiToken(token), tls, by: login::hint(), wait, machine };
        let open = dial::open(&target)?;
        // Every request's `by` must be who the connection is: the hub names that from the token,
        // whatever `by` the hello sent.
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
            held_on: open.welcome.machine,
            admin: open.welcome.admin,
        })
    }

    /// The machine this client's hello named: what a lease it takes is held on.
    pub fn machine(&self) -> Option<&wsp_hub_protocol::Machine> {
        self.target.machine.as_ref()
    }

    /// The machine the hub's `welcome` said it holds this connection's leases
    /// on: the hello's, where the hub keys leases by machine.
    pub fn held_on(&self) -> Option<&wsp_hub_protocol::Machine> {
        self.held_on.as_ref()
    }

    /// Whether the hub's `welcome` said this connection's user is a hub admin
    /// (the first one's); None from a hub older than the field, which does not say.
    pub fn admin(&self) -> Option<bool> {
        self.admin
    }

    /// The instance the hub named in its `welcome` (the first one's).
    pub fn server(&self) -> &str {
        &self.server
    }

    /// Who this connection is: the principal the hub's `welcome` named (from
    /// the token), else the hint the hello said. `wsp login` stores its name as
    /// the login's `user`.
    pub fn by(&self) -> &Principal {
        &self.target.by
    }

    /// The head the hub's `welcome` named when this client dialled.
    pub fn head(&self) -> EventId {
        self.head
    }

    /// The one timeout of a request, from sending it to its reply.
    pub fn set_timeout(&mut self, timeout: Duration) {
        self.timeout = timeout;
    }

    /// Send `call` and return the reply with the same id. A refusal is a
    /// [`Reply`] too: its outcome is `err`. After a lost connection the next
    /// call dials again.
    pub fn request(&mut self, call: Call) -> Result<Reply, Error> {
        self.send_call(call, None)
    }

    /// [`Client::request`] for a command: it carries the idempotency `key`, and
    /// a retry with the same key gets the first result.
    pub fn request_keyed(&mut self, call: Call, key: IdempotencyKey) -> Result<Reply, Error> {
        self.send_call(call, Some(key))
    }

    /// The `whoami` verb: who the hub holds this connection as (what the
    /// token names), and the instance it is on.
    pub fn whoami(&mut self) -> Result<(Principal, String), Error> {
        let reply = self.send_call(Call::Verb(Verb::Whoami(NoArgs {})), None)?;
        match reply.outcome {
            Outcome::Ok(Answer::Whoami { by, server }) => Ok((by, server)),
            Outcome::Ok(other) => Err(Error::Unexpected(format!("whoami was answered {other:?}"))),
            Outcome::Err(f) => Err(refused(f)),
        }
    }

    /// The `state` verb: the hub's whole state (its users, token records,
    /// shared workspaces and members) and the id it is consistent at.
    pub fn state(&mut self) -> Result<(State, EventId), Error> {
        let reply = self.send_call(Call::Verb(Verb::State(NoArgs {})), None)?;
        match reply.outcome {
            Outcome::Ok(Answer::State { state, at }) => Ok((state, at)),
            Outcome::Ok(other) => Err(Error::Unexpected(format!("state was answered {other:?}"))),
            Outcome::Err(f) => Err(refused(f)),
        }
    }

    /// Apply the identity command `cmd`: send it under a key minted for this
    /// call, and after a lost reply or a lost connection, send it again under
    /// the same key, so that a command the hub did apply is not applied twice
    /// and the retry gets the first result. All of it is within the one
    /// timeout ([`Client::set_timeout`]), at most [`APPLY_ATTEMPTS`] sends.
    /// The answer is the ids of the events it wrote.
    ///
    /// A refusal is the hub's [`Refusal`], as [`Error::Refused`], and is never
    /// retried; nor is any other `err` reply ([`Error::Failed`]) or a hub that
    /// cannot be dialled before anything was sent ([`Error::Silent`]).
    pub fn apply(&mut self, cmd: &IdentityCommand) -> Result<Vec<EventId>, Error> {
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
            match self.request_keyed(Call::Command(cmd.clone()), key.clone()) {
                Ok(reply) => {
                    break match reply.outcome {
                        Outcome::Ok(Answer::Applied) => Ok(reply.events),
                        Outcome::Ok(other) => Err(Error::Unexpected(format!("a command was answered {other:?}"))),
                        Outcome::Err(f) => Err(refused(f)),
                    }
                }
                Err(e @ (Error::Timeout | Error::Lost(_))) => {
                    sent = true;
                    last = e;
                }
                // Nothing went out on a connection that could not be made: the
                // first time, that is the answer; after a send, the hub may be
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

    /// A feed of the hub's stream from `from`: the events after that id, or,
    /// with `None`, the snapshot and then every event after it. It dials on its
    /// own connection when first read, and again whenever that one is lost.
    pub fn subscribe(&self, from: Option<EventId>) -> Feed {
        Feed::new(self.target.clone(), from, self.timeout)
    }
}

/// A key no other call has minted: the clock, the process, a count and what the
/// standard library's per-process randomness makes of them. As unique as a
/// retry needs it to be.
fn mint_key() -> IdempotencyKey {
    use std::collections::hash_map::RandomState;
    use std::hash::{BuildHasher, Hasher};
    use std::sync::atomic::{AtomicU64, Ordering};
    static COUNT: AtomicU64 = AtomicU64::new(0);
    let nanos = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_nanos()).unwrap_or_default();
    let mut h = RandomState::new().build_hasher();
    h.write_u128(nanos);
    h.write_u32(std::process::id());
    h.write_u64(COUNT.fetch_add(1, Ordering::Relaxed));
    IdempotencyKey(format!("{nanos:x}-{:x}-{:016x}", std::process::id(), h.finish()))
}
