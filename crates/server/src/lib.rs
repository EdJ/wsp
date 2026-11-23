//! `wsp-server`: the state core's server (docs/state-core.md section 6), with
//! only what step 1 serves: the read verbs of 4.2 and `subscribe` (4.3), on TCP
//! on loopback and the tailnet from the start (D19, agent-manager 3.4), over
//! any `dyn Store`, and [`mirror::Mirror`], the domain step 1's store runs.
//! Step 2 (wsp-121) adds [`domain::Composed`], the mirror and the orders
//! machine under a [`domain::Mode`], and the orders machine's commands over
//! the API (`conn`'s module docs): `order.send` and `order.cancel` from a
//! person or a seat, the answers and `order.deliver` from the instance itself
//! or a manager. Every other command verb is answered `unsupported` until its
//! step. See `MAP.md`.
//!
//! **Listening** ([`Server::start`]). It binds loopback and the tailnet
//! addresses it is given, on one port; it refuses any address, and any peer,
//! outside `127.0.0.0/8`, `::1`, `100.64.0.0/10` and `fd7a:115c:a1e0::/48`
//! ([`allowed`], the manager's rule). At first start it makes
//! `$state_dir/api.token` (32 random bytes as hex, 0600), narrowing it back,
//! and logging that, when it finds it wider. It refuses to start while
//! anything answers its port, and names what does. Once bound it writes
//! `$state_dir/api.json` ([`wsp_api::Listen`]), loopback first.
//!
//! **TLS** (wsp-461). The hub sets [`Config::tls`] and every connection it
//! accepts is inside a TLS session (`tls`, `tcp::Tcp::accept_tls`); the
//! daemon sets none. The peer rule is the same either way.
//!
//! **Per connection** (`conn`): `hello` first, or the connection is closed;
//! then requests answered by id. **Subscribe** (`subscribe`): a snapshot or a
//! replay, then the head followed through `wait_for`, with a beat. The
//! carrier is [`tcp::Tcp`].
//!
//! It goes through `Store` only, and links `wsp-api`, `wsp-store-contract` and
//! `wsp-machines` alone of the workspace (never the sqlite crate: section 6).

use std::fs::{self, OpenOptions};
use std::io::{self, Read, Write};
use std::net::{IpAddr, Ipv4Addr, SocketAddr, TcpListener, TcpStream};
use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use serde_json::Value;
use wsp_api::{
    ApiToken, Call, Code, Command, Connection, Failure, Frame, Hello, Listen, Outcome, Principal, SyncCall, LISTEN_FILE,
    PROTOCOL,
    TOKEN_FILE, TOKEN_MODE,
};
use wsp_store_contract::{At, Store};

mod conn;
pub mod domain;
pub mod mirror;
pub mod ranges;
mod subscribe;
pub mod tcp;
pub mod tls;
#[cfg(feature = "test-support")]
pub mod testing;

pub use domain::{Composed, Mode, OWNED};
pub use mirror::Mirror;
pub use ranges::Ranges;

/// The store a server serves: any backend, shared with its threads.
pub type SharedStore = Arc<dyn Store + Send + Sync>;

/// The port the server listens on unless told otherwise: the one after the
/// agent manager's (7487).
pub const DEFAULT_PORT: u16 = 7488;

/// The server's log, under `state_dir`: one line per start, refusal, and
/// connection that ends other than by its client.
pub const LOG_FILE: &str = "api.log";

/// The CLI's documents the view verbs answer (`throttle`, `spend`, `show`;
/// wsp-307): what `wsp throttle --json`, `wsp spend --json` and `wsp show <id>
/// --json` print, as the value they print. Their inputs are this machine's
/// readings, the stop ledger and the store's files, none of which a `dyn
/// Store` holds, so whatever starts the server (the daemon) hands in the one
/// code the CLI prints from, and the server, linking nothing of the CLI,
/// passes its answer through whole.
///
/// Each is called on the asking connection's thread, after its hello and the
/// request's `by` were checked as every read's are. An `Err` is the reply's
/// `err` as it stands (`not_found` for a `show` the CLI would say has none).
pub trait Views: Send + Sync {
    fn throttle(&self) -> Result<Value, Failure>;
    fn spend(&self) -> Result<Value, Failure>;
    /// `id` as `wsp show` takes it: a row's id or prefix, or a piece's id.
    fn show(&self, id: &str) -> Result<Value, Failure>;
}

impl std::fmt::Debug for dyn Views {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("Views(..)")
    }
}

/// What answers the sync verbs (wsp-459: `sync.heads`, `sync.have`, `sync.put`,
/// `sync.get`, `refs.move`), which the hub (wsp-460) serves and a state core
/// does not. Like [`Views`], it is handed in, since what holds the objects is
/// no `dyn Store`: the server passes the call and the connection's principal in
/// and the outcome back whole, on the asking connection's thread, after the
/// hello and the request's `by` were checked.
pub trait SyncVerbs: Send + Sync {
    fn answer(&self, by: &Principal, call: SyncCall) -> Outcome;
}

impl std::fmt::Debug for dyn SyncVerbs {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("SyncVerbs(..)")
    }
}

/// Who a connection is, where the hello's token decides it rather than its
/// `by` (the hub, wsp-161). Without one a server keeps the state core's rule:
/// the hello's token must be the instance's, and its `by` is trusted. Like
/// [`SyncVerbs`] it is handed in, and the server holds nothing of what it
/// decides from.
pub trait Gate: Send + Sync {
    /// The principal `hello`'s token names, which replaces the hello's `by` for
    /// the connection's life (and is what `welcome` names); `Err` is refused
    /// `unauthorised` with its text. `token` is the instance's own
    /// (`api.token`), for a gate that gives it a meaning.
    fn hello(&self, hello: &Hello, token: &ApiToken) -> Result<Principal, String>;
    /// Whether the connection's principal `by` may make `call`, asked after
    /// the request's `by` was found to be `by`; `Err` is the reply's `err`.
    fn admits(&self, by: &Principal, call: &Call) -> Result<(), Failure>;
    /// The commands applied through the store beside the orders machine's
    /// (with a key, under the connection's principal), whose domain decides
    /// who may send them.
    fn owns(&self, cmd: &Command) -> bool;
}

impl std::fmt::Debug for dyn Gate {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("Gate(..)")
    }
}

/// What a server is started with.
#[derive(Debug, Clone)]
pub struct Config {
    /// The tailnet addresses it binds beside loopback (which it always binds).
    pub addrs: Vec<IpAddr>,
    /// Where `api.token`, `api.json` and `api.log` live (`$WSP_STATE`).
    pub state_dir: PathBuf,
    /// `0` takes an ephemeral port (tests), the same one on every address.
    pub port: u16,
    /// The instance it serves, as `welcome` names it.
    pub instance: String,
    /// The beat's period, [`wsp_api::BEAT_SECS`] outside tests. A client
    /// silent for [`wsp_api::MISSED_BEATS`] of them is dropped.
    pub beat: Duration,
    /// What answers the view verbs; none answers each `unsupported`.
    pub views: Option<Arc<dyn Views>>,
    /// What answers the sync verbs; none answers each `unsupported`.
    pub sync: Option<Arc<dyn SyncVerbs>>,
    /// Who a hello is; none trusts the hello's `by` behind the instance's token.
    pub gate: Option<Arc<dyn Gate>>,
    /// The ranges a peer must be in, and an address to bind (loopback aside,
    /// which is always bound): none keeps [`allowed`]'s loopback and tailnet.
    pub peers: Option<Ranges>,
    /// TLS for every accepted connection (the hub's, wsp-461: [`tls::server_config`]);
    /// none keeps plain TCP, which is the daemon's. It does not change who is
    /// admitted: [`Config::admits`] reads `peers` or [`allowed`] either way.
    pub tls: Option<Arc<rustls::ServerConfig>>,
}

impl Config {
    /// Loopback alone, on [`DEFAULT_PORT`], instance `wsp`, a 10 s beat, no
    /// [`Views`].
    pub fn new(state_dir: &Path) -> Config {
        Config {
            addrs: Vec::new(),
            state_dir: state_dir.to_path_buf(),
            port: DEFAULT_PORT,
            instance: "wsp".into(),
            beat: Duration::from_secs(wsp_api::BEAT_SECS),
            views: None,
            sync: None,
            gate: None,
            peers: None,
            tls: None,
        }
    }

    /// Whether a peer at `ip` may connect, or the server bind it: [`Config::peers`]
    /// when it names ranges, else [`allowed`].
    pub fn admits(&self, ip: &IpAddr) -> bool {
        match &self.peers {
            Some(ranges) => ranges.contains(ip),
            None => allowed(ip),
        }
    }
}

/// Why a server did not start.
#[derive(Debug)]
pub enum StartError {
    /// An address outside loopback and the tailnet ([`allowed`]).
    Address(IpAddr),
    /// Something already answers at `addr`: `who` names it.
    Answered { addr: SocketAddr, who: String },
    /// The state directory, the token, a bind or `api.json` failed.
    Io(String),
}

impl std::fmt::Display for StartError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            StartError::Address(ip) => write!(f, "{ip} is outside the ranges this server admits (loopback and the tailnet unless configured)"),
            StartError::Answered { addr, who } => write!(f, "{who} answers at {addr}"),
            StartError::Io(why) => f.write_str(why),
        }
    }
}

impl std::error::Error for StartError {}

fn io_err(doing: &str, e: io::Error) -> StartError {
    StartError::Io(format!("{doing}: {e}"))
}

/// Whether a peer may connect from `ip`, or the server bind it: loopback, or
/// the tailnet's ranges (`100.64.0.0/10`, `fd7a:115c:a1e0::/48`), and nothing
/// else. The manager's rule (`wsp-manager`'s `allowed`), copied: this crate
/// may not link the manager. An IPv4 address mapped into IPv6 is read as the
/// IPv4 one.
pub fn allowed(ip: &IpAddr) -> bool {
    match ip.to_canonical() {
        IpAddr::V4(v4) => v4.is_loopback() || (v4.octets()[0] == 100 && (v4.octets()[1] & 0xc0) == 64),
        IpAddr::V6(v6) => {
            let s = v6.segments();
            v6.is_loopback() || (s[0] == 0xfd7a && s[1] == 0x115c && s[2] == 0xa1e0)
        }
    }
}

/// The instance's token at `path`, made there first if there is none: 32
/// random bytes as hex, mode [`TOKEN_MODE`]. A token file others could read is
/// narrowed back, and the second value says so, for the log.
pub fn ensure_token(path: &Path) -> io::Result<(ApiToken, bool)> {
    const BYTES: usize = 32;
    match OpenOptions::new().write(true).create_new(true).mode(TOKEN_MODE).open(path) {
        Ok(mut f) => {
            let mut raw = [0u8; BYTES];
            fs::File::open("/dev/urandom")?.read_exact(&mut raw)?;
            let hex: String = raw.iter().map(|b| format!("{b:02x}")).collect();
            f.write_all(hex.as_bytes())?;
            f.write_all(b"\n")?;
            f.sync_all()?;
            Ok((ApiToken(hex), false))
        }
        Err(e) if e.kind() == io::ErrorKind::AlreadyExists => {
            let mode = fs::metadata(path)?.permissions().mode() & 0o777;
            let narrowed = mode != TOKEN_MODE;
            if narrowed {
                fs::set_permissions(path, fs::Permissions::from_mode(TOKEN_MODE))?;
            }
            let text = fs::read_to_string(path)?;
            let token = text.trim();
            if token.is_empty() {
                return Err(io::Error::other(format!("{} is empty; remove it to make a new token", path.display())));
            }
            Ok((ApiToken(token.to_string()), narrowed))
        }
        Err(e) => Err(e),
    }
}

/// Now, in milliseconds since the epoch.
pub(crate) fn now() -> At {
    At(SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_millis() as u64).unwrap_or(0))
}

/// The server's log: a line per event worth keeping, appended to `api.log`.
pub(crate) struct Log {
    path: PathBuf,
}

impl Log {
    pub(crate) fn line(&self, what: &str) {
        if let Ok(mut f) = OpenOptions::new().create(true).append(true).open(&self.path) {
            let _ = writeln!(f, "{} {what}", now().0);
        }
    }
}

/// What every thread of a server shares.
pub(crate) struct Shared {
    pub(crate) store: SharedStore,
    pub(crate) cfg: Config,
    pub(crate) token: ApiToken,
    pub(crate) log: Log,
    pub(crate) stopping: AtomicBool,
    /// The welcomed connections, by number, with a handle that closes each.
    pub(crate) clients: Mutex<std::collections::BTreeMap<u64, TcpStream>>,
    next_client: AtomicU64,
}

impl Shared {
    pub(crate) fn stopping(&self) -> bool {
        self.stopping.load(Ordering::SeqCst)
    }
}

/// A running server. Dropping it does not stop it; [`Server::stop`] does.
pub struct Server {
    shared: Arc<Shared>,
    addrs: Vec<SocketAddr>,
    threads: Vec<JoinHandle<()>>,
}

impl Server {
    /// Start serving `store`: make or read the token, refuse if anything
    /// answers the port, bind loopback and `cfg.addrs`, write `api.json`, and
    /// accept.
    pub fn start(store: SharedStore, cfg: Config) -> Result<Server, StartError> {
        if let Some(bad) = cfg.addrs.iter().find(|ip| !cfg.admits(ip)) {
            return Err(StartError::Address(*bad));
        }
        let mut ips = vec![IpAddr::V4(Ipv4Addr::LOCALHOST)];
        for ip in &cfg.addrs {
            if !ips.contains(ip) {
                ips.push(*ip);
            }
        }
        fs::create_dir_all(&cfg.state_dir).map_err(|e| io_err("making the state dir", e))?;
        let log = Log { path: cfg.state_dir.join(LOG_FILE) };
        let (token, narrowed) =
            ensure_token(&cfg.state_dir.join(TOKEN_FILE)).map_err(|e| io_err("making the token", e))?;
        if narrowed {
            log.line(&format!("{TOKEN_FILE} was readable by others; narrowed to 0600"));
        }

        if cfg.port != 0 {
            for ip in &ips {
                let addr = SocketAddr::new(*ip, cfg.port);
                if let Some(who) = answers(addr, &token) {
                    log.line(&format!("refused to start: {who} at {addr}"));
                    return Err(StartError::Answered { addr, who });
                }
            }
        }
        let mut listeners = Vec::new();
        let mut port = cfg.port;
        for ip in &ips {
            let addr = SocketAddr::new(*ip, port);
            let l = match TcpListener::bind(addr) {
                Err(e) if !ip.is_loopback() && e.kind() == io::ErrorKind::AddrNotAvailable => {
                    log.line(&format!("not on the tailnet: cannot bind {addr}: {e}; serving the other addresses"));
                    continue;
                }
                bound => bound,
            };
            let l = l.map_err(|e| {
                log.line(&format!("refused to start: cannot bind {addr}: {e}"));
                if e.kind() == io::ErrorKind::AddrInUse {
                    StartError::Answered { addr, who: format!("something (the port is in use: {e})") }
                } else {
                    io_err(&format!("binding {addr}"), e)
                }
            })?;
            port = l.local_addr().map_err(|e| io_err("reading the bound port", e))?.port();
            l.set_nonblocking(true).map_err(|e| io_err("setting the listener non-blocking", e))?;
            listeners.push(l);
        }
        let addrs: Vec<SocketAddr> = listeners.iter().filter_map(|l| l.local_addr().ok()).collect();
        write_listen(&cfg.state_dir, &addrs).map_err(|e| io_err(&format!("writing {LISTEN_FILE}"), e))?;
        log.line(&format!(
            "started: instance {}, protocol {PROTOCOL}, on {}",
            cfg.instance,
            addrs.iter().map(ToString::to_string).collect::<Vec<_>>().join(" ")
        ));

        let shared = Arc::new(Shared {
            store,
            cfg,
            token,
            log,
            stopping: AtomicBool::new(false),
            clients: Mutex::new(Default::default()),
            next_client: AtomicU64::new(1),
        });
        let threads = listeners
            .into_iter()
            .map(|l| {
                let s = shared.clone();
                std::thread::spawn(move || accept(s, l))
            })
            .collect();
        Ok(Server { shared, addrs, threads })
    }

    /// The addresses it listens on, with the port it bound, loopback first.
    pub fn addrs(&self) -> &[SocketAddr] {
        &self.addrs
    }

    /// The instance's token, as `api.token` holds it.
    pub fn token(&self) -> &ApiToken {
        &self.shared.token
    }

    /// How many clients have been welcomed and are still connected.
    pub fn clients(&self) -> usize {
        self.shared.clients.lock().unwrap_or_else(|e| e.into_inner()).len()
    }

    /// The log's path.
    pub fn log_path(&self) -> PathBuf {
        self.shared.log.path.clone()
    }

    /// Stop accepting and close every client; their subscriptions end with
    /// them. The store is left as it is, for the next start.
    pub fn stop(self) {
        self.shared.stopping.store(true, Ordering::SeqCst);
        for t in self.threads {
            let _ = t.join();
        }
        let clients = std::mem::take(&mut *self.shared.clients.lock().unwrap_or_else(|e| e.into_inner()));
        for s in clients.values() {
            let _ = s.shutdown(std::net::Shutdown::Both);
        }
        self.shared.log.line("stopped");
    }
}

/// `api.json`, written whole or not at all (a temporary file, then a rename).
fn write_listen(dir: &Path, addrs: &[SocketAddr]) -> io::Result<()> {
    let listen = Listen { addrs: addrs.iter().map(ToString::to_string).collect() };
    let text = serde_json::to_string_pretty(&listen).map_err(io::Error::other)?;
    let tmp = dir.join(format!("{LISTEN_FILE}.tmp"));
    fs::write(&tmp, text + "\n")?;
    fs::rename(&tmp, dir.join(LISTEN_FILE))
}

/// What answers at `addr`, if anything: a hello with this instance's token,
/// and what came back.
fn answers(addr: SocketAddr, token: &ApiToken) -> Option<String> {
    let mut conn = tcp::Tcp::dial(addr, Duration::from_millis(500)).ok()?;
    let hello = Frame::Hello(Hello {
        token: token.clone(),
        by: Principal::Wsp("start-check".into()),
        client: format!("wsp-server {} (start check)", env!("CARGO_PKG_VERSION")),
        protocol: PROTOCOL,
    });
    let _ = conn.send(&hello);
    let said = conn.recv(Duration::from_secs(2));
    conn.shutdown();
    Some(match said {
        Ok(Some(Frame::Welcome(w))) => {
            format!("a wsp server with this token (instance {}, protocol {}, head {})", w.server, w.protocol, w.head.0)
        }
        Ok(Some(Frame::Refused(r))) if r.code == Code::Unauthorised => "a wsp server with another token".into(),
        Ok(Some(Frame::Refused(r))) => format!("a wsp server that refused the hello ({:?}: {})", r.code, r.text),
        Ok(Some(_)) | Err(wsp_api::ConnError::Malformed(_)) => "something that is not a wsp server".into(),
        Ok(None) | Err(_) => "something that does not answer a hello".into(),
    })
}

/// Accept until the server stops. The listener is non-blocking, so a stop is
/// seen within a poll.
fn accept(shared: Arc<Shared>, listener: TcpListener) {
    const POLL: Duration = Duration::from_millis(25);
    while !shared.stopping() {
        match listener.accept() {
            Ok((stream, peer)) => {
                if !shared.cfg.admits(&peer.ip()) {
                    shared.log.line(&format!("{peer}: refused, outside the ranges this server admits"));
                    drop(stream);
                    continue;
                }
                // An accepted stream may inherit the listener's non-blocking mode.
                if stream.set_nonblocking(false).is_err() {
                    continue;
                }
                let n = shared.next_client.fetch_add(1, Ordering::SeqCst);
                let s = shared.clone();
                std::thread::spawn(move || conn::serve(s, stream, n));
            }
            Err(e) if e.kind() == io::ErrorKind::WouldBlock => std::thread::sleep(POLL),
            Err(e) if e.kind() == io::ErrorKind::Interrupted => {}
            Err(e) => {
                shared.log.line(&format!("accept failed: {e}"));
                std::thread::sleep(POLL);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// An address in a range the tests name by octets, so no literal of one sits in the tree.
    fn ip4(a: u8, b: u8, rest: &str) -> String {
        format!("{a}.{b}.{rest}")
    }

    #[test]
    fn a_peer_outside_loopback_and_the_tailnet_is_refused() {
        for ok in [
            "127.0.0.1".to_string(),
            "127.255.0.9".to_string(),
            "::1".to_string(),
            ip4(100, 64, "0.1"),
            ip4(100, 101, "102.103"),
            ip4(100, 127, "255.254"),
            "fd7a:115c:a1e0::1".to_string(),
            "::ffff:127.0.0.1".to_string(),
        ] {
            assert!(allowed(&ok.parse().unwrap()), "{ok} is allowed");
        }
        for no in [
            "10.0.0.1".to_string(),
            ip4(192, 168, "1.2"),
            "100.63.255.255".to_string(),
            "100.128.0.1".to_string(),
            "8.8.8.8".to_string(),
            "0.0.0.0".to_string(),
            "::".to_string(),
            "fd7a:115c:a1e1::1".to_string(),
            "fe80::1".to_string(),
            "::ffff:10.0.0.1".to_string(),
        ] {
            assert!(!allowed(&no.parse().unwrap()), "{no} is refused");
        }
    }

    #[test]
    fn the_token_is_made_once_at_0600_and_narrowed_back_when_widened() {
        let d = std::env::temp_dir().join(format!("wsp-server-token-{}", std::process::id()));
        let _ = fs::remove_dir_all(&d);
        fs::create_dir_all(&d).unwrap();
        let p = d.join(TOKEN_FILE);
        let (made, narrowed) = ensure_token(&p).unwrap();
        assert!(!narrowed);
        assert_eq!(made.0.len(), 64);
        assert!(made.0.bytes().all(|b| b.is_ascii_hexdigit()));
        assert_eq!(fs::metadata(&p).unwrap().permissions().mode() & 0o777, 0o600);
        let (again, _) = ensure_token(&p).unwrap();
        assert_eq!(made, again, "a restart keeps the token clients hold");

        fs::set_permissions(&p, fs::Permissions::from_mode(0o644)).unwrap();
        let (_, narrowed) = ensure_token(&p).unwrap();
        assert!(narrowed);
        assert_eq!(fs::metadata(&p).unwrap().permissions().mode() & 0o777, 0o600);
        let _ = fs::remove_dir_all(&d);
    }

    #[test]
    fn an_address_outside_the_rule_is_not_bound() {
        let d = std::env::temp_dir().join(format!("wsp-server-addr-{}", std::process::id()));
        let mut cfg = Config::new(&d);
        cfg.addrs = vec![ip4(192, 168, "1.2").parse().unwrap()];
        let store: SharedStore = Arc::new(NoStore);
        assert!(matches!(Server::start(store, cfg), Err(StartError::Address(_))));
        let _ = fs::remove_dir_all(&d);
    }

    #[test]
    fn a_tailnet_address_this_machine_does_not_hold_is_skipped_and_loopback_still_serves() {
        let d = std::env::temp_dir().join(format!("wsp-server-unheld-{}", std::process::id()));
        let mut cfg = Config::new(&d);
        let unheld: IpAddr = ip4(100, 127, "255.254").parse().unwrap();
        cfg.addrs = vec![unheld];
        cfg.port = 0; // the live server on DEFAULT_PORT is no part of this test
        let store: SharedStore = Arc::new(NoStore);
        let server = Server::start(store, cfg).expect("loopback serves when a tailnet address cannot be bound");
        assert!(server.addrs().iter().all(|a| a.ip().is_loopback()), "only loopback is bound: {:?}", server.addrs());
        assert!(d.join(LISTEN_FILE).exists(), "{LISTEN_FILE} is written");
        let log = fs::read_to_string(d.join(LOG_FILE)).unwrap();
        assert!(log.contains("not on the tailnet") && log.contains(&unheld.to_string()), "the skip is logged: {log}");
        server.stop();
        let _ = fs::remove_dir_all(&d);
    }

    /// A store no test reaches: the start is refused before it is used, or no client connects.
    struct NoStore;
    macro_rules! never {
        () => {
            unreachable!("the start was refused before the store was used")
        };
    }
    use wsp_store_contract::*;
    impl Reads for NoStore {
        fn row(&self, _: &RowId) -> Result<Option<Row>> { never!() }
        fn rows(&self, _: &RowFilter) -> Result<Vec<Row>> { never!() }
        fn seat(&self, _: &SeatId) -> Result<Option<Seat>> { never!() }
        fn seats(&self, _: &SeatFilter) -> Result<Vec<Seat>> { never!() }
        fn list(&self, _: &ListId) -> Result<Option<List>> { never!() }
        fn orders_for(&self, _: &SeatId, _: &[OrderState]) -> Result<Vec<Order>> { never!() }
        fn claim(&self, _: &RowId) -> Result<Option<Claim>> { never!() }
        fn lease(&self, _: &LeaseName) -> Result<Option<Lease>> { never!() }
        fn machine(&self, _: &MachineName) -> Result<Option<Machine>> { never!() }
        fn snapshot(&self) -> Result<(State, EventId)> { never!() }
        fn wip_snapshot(&self) -> Result<(State, EventId)> { never!() }
    }
    impl Events for NoStore {
        fn events_after(&self, _: EventId, _: usize) -> Result<Vec<Event>> { never!() }
        fn head(&self) -> Result<EventId> { never!() }
        fn wait_for(&self, _: EventId, _: Duration) -> Result<EventId> { never!() }
    }
    impl Commands for NoStore {
        fn apply(&self, _: Command, _: IdempotencyKey, _: Principal) -> Result<Applied> { never!() }
    }
    impl Leases for NoStore {
        fn take(&self, _: &LeaseName, _: &Principal, _: Duration) -> Result<Option<Token>> { never!() }
        fn renew(&self, _: &LeaseName, _: Token, _: Duration) -> Result<bool> { never!() }
        fn release(&self, _: &LeaseName, _: Token) -> Result<()> { never!() }
    }
    impl Claims for NoStore {
        fn claim(&self, _: &RowId, _: &SeatId, _: Duration, _: Option<&Fence>) -> Result<Token> { never!() }
        fn release(&self, _: &RowId, _: Token) -> Result<()> { never!() }
    }
    impl Readings for NoStore {}
    impl Texts for NoStore {
        fn text(&self, _: &RowId, _: &str) -> Result<(String, Rev)> { never!() }
        fn set_text(&self, _: &RowId, _: &str, _: String, _: Rev) -> Result<Rev> { never!() }
    }
}
