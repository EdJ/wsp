//! `wsp-manager`: the agent manager (`docs/agent-manager.md`, step M1 of 6.2;
//! wsp-021); see `MAP.md`.
//!
//! One long-lived process per host:
//! - it listens on TCP, on loopback and the host's tailnet addresses and
//!   nothing else (d1 Q5), and refuses to start while another manager answers
//!   its port or holds its `<exec_dir>` (2.4);
//! - it makes its token at first start, 0600 in `<exec_dir>`, and closes a
//!   connection whose `hello` does not carry it (3.3);
//! - it answers `hello` with its host id, version, compound-sup and
//!   compound-render wire range, journal head, seats and capacity (3.2);
//! - it appends every event to `<exec_dir>/journal.jsonl` before sending it,
//!   ids a per-manager sequence, and serves `subscribe {from}` from the
//!   journal (2.5, 3.1);
//! - it beats every [`BEAT_SECS`] on its own timer with the host's facts, read
//!   once per beat through `wsp-host` for every client, and drops a client it
//!   has not heard for [`MISSED_BEATS`](wsp_host_api::MISSED_BEATS) beats (3.4);
//! - it takes the hooks its seats post ([`wsp_host::hooks`]: loopback, the
//!   token) and journals what they say: `turn`, `seat.said`, `seat.spent` and
//!   `seat.did` (wsp-023);
//! - it logs to `<exec_dir>/log/` ([`log`]);
//! - it runs seats (wsp-022, [`seats`]): `seat.spawn` keyed by `(row,
//!   attempt)`, forking one compound-sup per seat; `seat.stop`; orders through
//!   compound-sup's `deliver <id>`; `seats` and `seat.read`; seat epochs; and
//!   on restart the seat table rebuilt from the sessions in `<exec_dir>/run`,
//!   with an order in flight asked again under its id (2.5, 3.3);
//! - it runs tree jobs (wsp-050, [`jobs`]): `job.run` keyed by
//!   [`JobKey`](wsp_host_api::JobKey) under a hard cap per class, `job.stop` on
//!   the job's own pgid, `jobs`, an undetached job stopped with its
//!   connection, and on restart the jobs re-adopted through `wsp-host`'s exit
//!   files (6.2 M3).
//!
//! It depends on `wsp-host-api`, `wsp-base` and `wsp-host`, and on nothing of
//! `wsp-store` (2.3 R1, d1 Q3).

mod jobs;
pub mod journal;
pub mod log;
pub mod seats;
mod serve;
pub mod token;

use std::fs::{self, File, OpenOptions, TryLockError};
use std::io::Write;
use std::net::{IpAddr, Ipv4Addr, SocketAddr, TcpListener, TcpStream};
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Condvar, Mutex};
use std::thread::JoinHandle;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use wsp_host::Tcp;
use wsp_host_api::{
    At, Beat, Capacity, Connection, EventId, Frame, Hello, HostId, Message, Role, Token, WspHello,
    BEAT_SECS, PROTOCOL,
};

pub use journal::Journal;

/// The port a manager listens on unless told otherwise. A machine record that
/// names another port (wsp-024, wsp-025) overrides it.
pub const DEFAULT_PORT: u16 = wsp_host::hooks::PORT;
/// This build's version, as `hello` reports it.
pub const VERSION: &str = env!("CARGO_PKG_VERSION");
/// Under `<exec_dir>`: the token, the journal, the lock and the log directory.
pub const TOKEN_FILE: &str = wsp_host::hooks::TOKEN_FILE;
pub const JOURNAL_FILE: &str = "journal.jsonl";
pub const LOCK_FILE: &str = "manager.lock";
pub const LOG_DIR: &str = "log";

/// How long [`Manager::stop`] waits for its listeners' addresses to stop
/// accepting once it has closed them (see [`Lock`] for why one may not).
const RELEASE_WAIT: Duration = Duration::from_secs(5);

/// What one manager is started with.
#[derive(Debug, Clone)]
pub struct Config {
    /// Where the token, journal, lock and log live.
    pub exec_dir: PathBuf,
    /// The name it gives itself in `hello`.
    pub host: HostId,
    /// The addresses it binds: loopback and tailnet ones only ([`allowed`]).
    pub addrs: Vec<IpAddr>,
    /// `0` takes an ephemeral port (tests), the same one on every address.
    pub port: u16,
    /// The beat's period, [`BEAT_SECS`] outside tests.
    pub beat: Duration,
    /// Paths whose volumes the beat reports disk free for.
    pub volumes: Vec<PathBuf>,
    /// The compound-sup it forks; `None` finds the installed one
    /// (`$COMPOUND_SUP`, `PATH`, `~/.local/bin`). A test names its fake.
    pub sup: Option<PathBuf>,
    /// How many seats it runs or starts at once, as `hello`'s capacity says;
    /// a spawn past it is refused `full`.
    pub seats: u32,
    /// The run directories of seats it did not start: the in-process host's
    /// (`$COMPOUND_RUN_DIR`, else `~/.compound/run`). At start, a labelled
    /// session there that answers is adopted (wsp-029, 6.2), as one under its
    /// own `<exec_dir>/run` is. Empty in [`Config::new`]: a test names its own.
    pub inproc_runs: Vec<PathBuf>,
    /// How many `build` jobs it runs at once, as `hello`'s capacity says; a
    /// `job.run` past it is refused `full`. A hard cap and nothing more: when
    /// to ask is the throttle's (d1 Q11).
    pub build: u32,
    /// The same for `check` jobs.
    pub check: u32,
    /// The host's tree root: a job's `dir` is a path under it.
    pub tree_root: PathBuf,
    /// How long `job.stop` waits after `TERM` before it sends `KILL`.
    pub job_grace: Duration,
    /// How long an ended job, its `job.ended` journalled, keeps its table
    /// entry and files for a retry of its key; past it both are pruned.
    pub job_retention: Duration,
}

/// The seats a manager admits unless told otherwise.
pub const DEFAULT_SEATS: u32 = 32;
/// The `build` and `check` jobs a manager runs at once unless told otherwise.
pub const DEFAULT_BUILD: u32 = 2;
pub const DEFAULT_CHECK: u32 = 4;
/// `job.stop`'s grace between `TERM` and `KILL`.
pub const DEFAULT_JOB_GRACE: Duration = Duration::from_secs(5);
/// How long an ended job is kept for a retry before it is pruned: a day.
pub const DEFAULT_JOB_RETENTION: Duration = Duration::from_secs(24 * 60 * 60);

impl Config {
    /// Loopback on [`DEFAULT_PORT`], a 10 s beat, disk free for `exec_dir`'s
    /// volume, the home directory as the tree root.
    pub fn new(exec_dir: &Path, host: &str) -> Config {
        Config {
            exec_dir: exec_dir.to_path_buf(),
            host: HostId(host.to_string()),
            addrs: vec![IpAddr::V4(Ipv4Addr::LOCALHOST)],
            port: DEFAULT_PORT,
            beat: Duration::from_secs(BEAT_SECS),
            volumes: vec![exec_dir.to_path_buf()],
            sup: None,
            seats: DEFAULT_SEATS,
            inproc_runs: Vec::new(),
            build: DEFAULT_BUILD,
            check: DEFAULT_CHECK,
            tree_root: wsp_base::util::home(),
            job_grace: DEFAULT_JOB_GRACE,
            job_retention: DEFAULT_JOB_RETENTION,
        }
    }
}

/// Whether the manager may bind `ip`: loopback, or the tailnet's ranges
/// (`100.64.0.0/10`, `fd7a:115c:a1e0::/48`), and nothing else (d1 Q5).
pub fn allowed(ip: &IpAddr) -> bool {
    match ip {
        IpAddr::V4(v4) => v4.is_loopback() || (v4.octets()[0] == 100 && (v4.octets()[1] & 0xc0) == 64),
        IpAddr::V6(v6) => {
            let s = v6.segments();
            v6.is_loopback() || (s[0] == 0xfd7a && s[1] == 0x115c && s[2] == 0xa1e0)
        }
    }
}

/// The tailnet addresses out of `tailscale ip`'s lines.
pub fn tailnet_addrs(out: &str) -> Vec<IpAddr> {
    out.lines().filter_map(|l| l.trim().parse::<IpAddr>().ok()).filter(|ip| allowed(ip) && !ip.is_loopback()).collect()
}

/// Why a manager did not start.
#[derive(Debug)]
pub enum StartError {
    /// Something already answers at this address; `who` says what it said.
    Answered { addr: SocketAddr, who: String },
    /// Another manager holds this `<exec_dir>`.
    Locked { dir: PathBuf, holder: String },
    /// An address that is neither loopback nor the tailnet's.
    Address(IpAddr),
    /// No address to bind.
    NoAddress,
    Io(String),
}

impl std::fmt::Display for StartError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            StartError::Answered { addr, who } => write!(f, "another process answers at {addr}: {who}"),
            StartError::Locked { dir, holder } => {
                write!(f, "another manager holds {} (pid {holder})", dir.display())
            }
            StartError::Address(ip) => write!(f, "{ip} is neither loopback nor a tailnet address; refusing to bind it"),
            StartError::NoAddress => f.write_str("no address to bind"),
            StartError::Io(why) => f.write_str(why),
        }
    }
}

impl std::error::Error for StartError {}

fn io(what: &str, e: std::io::Error) -> StartError {
    StartError::Io(format!("{what}: {e}"))
}

/// The milliseconds since the epoch, as the protocol's [`At`].
pub fn now() -> At {
    At(SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_millis() as u64).unwrap_or(0))
}

/// One connected client, as the beat and the journal see it.
pub(crate) struct Client {
    pub(crate) out: std::sync::mpsc::Sender<Frame>,
    pub(crate) subscribed: bool,
    /// A handle on the stream, to close it from outside its threads.
    pub(crate) closer: Tcp,
}

/// What the manager's threads share.
pub(crate) struct Shared {
    pub(crate) cfg: Config,
    pub(crate) token: Token,
    /// The loopback address it listens on, port included: where the hooks of
    /// a seat it forks post (`WSP_MANAGER_ADDR`), so a manager on a port of
    /// its own (wsp's in-process one, wsp-030) hears its seats' turns.
    pub(crate) hook_addr: Option<SocketAddr>,
    pub(crate) log: log::Log,
    /// Held while appending and while a subscriber's replay is queued, so an
    /// event is never both replayed and sent live, nor missed between.
    pub(crate) journal: Mutex<Journal>,
    pub(crate) clients: Mutex<std::collections::BTreeMap<u64, Client>>,
    pub(crate) next_client: AtomicU64,
    pub(crate) stopping: AtomicBool,
    /// Wakes the beat's sleep at stop.
    pub(crate) wake: (Mutex<bool>, Condvar),
    /// The last beat sent, queued again for a client just after its `hello`,
    /// so a one-shot call has the host's facts without waiting out a period.
    pub(crate) last_beat: Mutex<Option<Frame>>,
    /// Hooks into events; held across a post's emits, so one seat's events
    /// are journalled in the order its hooks arrived.
    pub(crate) hooks: Mutex<wsp_host::hooks::Fold>,
    /// The seats it runs, their keys and epochs, and the orders it was asked.
    /// Taken before the journal, never while holding it.
    pub(crate) seats: Mutex<seats::Table>,
    /// The jobs it runs and ran. Taken before the journal, never while
    /// holding it.
    pub(crate) jobs: Mutex<jobs::Table>,
}

impl Shared {
    /// Journal `msg`, then send it to every subscriber.
    pub(crate) fn emit(&self, msg: Message) -> std::io::Result<EventId> {
        self.emit_for(msg, None)
    }

    /// Journal `msg`, then send it to every subscriber, and to whoever asked
    /// for it with their `req`: once, whether or not they subscribe.
    pub(crate) fn emit_for(&self, msg: Message, asker: Option<&seats::Asker>) -> std::io::Result<EventId> {
        let mut journal = self.journal.lock().unwrap_or_else(|e| e.into_inner());
        let frame = journal.append(msg)?;
        let id = frame.id.expect("the journal ids every frame");
        let clients = self.clients.lock().unwrap_or_else(|e| e.into_inner());
        let asking = asker.and_then(|a| a.out.as_ref().and(a.client));
        for (_, c) in clients.iter().filter(|(n, c)| c.subscribed && Some(**n) != asking) {
            let _ = c.out.send(frame.clone());
        }
        if let Some(a) = asker {
            a.send_frame(frame);
        }
        Ok(id)
    }

    /// compound-sup's run directory: the seats' session sockets.
    pub(crate) fn run_dir(&self) -> PathBuf {
        self.cfg.exec_dir.join(seats::RUN_DIR)
    }

    pub(crate) fn capacity(&self) -> Capacity {
        Capacity { seats: self.cfg.seats, build: self.cfg.build, check: self.cfg.check }
    }
}

/// A running manager. Dropping it does not stop it; [`Manager::stop`] does.
pub struct Manager {
    shared: Arc<Shared>,
    addrs: Vec<SocketAddr>,
    threads: Vec<JoinHandle<()>>,
    /// The `<exec_dir>` lock, held for the manager's life.
    _lock: Lock,
}

impl Manager {
    /// Start a manager: lock `<exec_dir>`, make or read its token, refuse if
    /// anything answers its port, bind, open the journal, and begin beating.
    pub fn start(cfg: Config) -> Result<Manager, StartError> {
        if cfg.addrs.is_empty() {
            return Err(StartError::NoAddress);
        }
        if let Some(bad) = cfg.addrs.iter().find(|ip| !allowed(ip)) {
            return Err(StartError::Address(*bad));
        }
        fs::create_dir_all(&cfg.exec_dir).map_err(|e| io("making the exec dir", e))?;
        let _ = fs::set_permissions(&cfg.exec_dir, fs::Permissions::from_mode(0o700));
        let log = log::Log::open(&cfg.exec_dir.join(LOG_DIR)).map_err(|e| io("opening the log", e))?;

        let lock = lock(&cfg.exec_dir)?;
        let (token, narrowed) =
            token::ensure(&cfg.exec_dir.join(TOKEN_FILE)).map_err(|e| io("making the token", e))?;
        if narrowed {
            log.line(&format!("{TOKEN_FILE} was readable by others; narrowed to 0600"));
        }

        if cfg.port != 0 {
            for ip in &cfg.addrs {
                let addr = SocketAddr::new(*ip, cfg.port);
                if let Some(who) = answers(addr, &token) {
                    log.line(&format!("refused to start: {who} at {addr}"));
                    return Err(StartError::Answered { addr, who });
                }
            }
        }
        let mut listeners = Vec::new();
        let mut port = cfg.port;
        for ip in &cfg.addrs {
            let addr = SocketAddr::new(*ip, port);
            let l = match TcpListener::bind(addr) {
                Err(e) if !ip.is_loopback() && e.kind() == std::io::ErrorKind::AddrNotAvailable => {
                    log.line(&format!("not on the tailnet: cannot bind {addr}: {e}; serving the other addresses"));
                    continue;
                }
                bound => bound,
            };
            let l = l.map_err(|e| {
                log.line(&format!("refused to start: cannot bind {addr}: {e}"));
                if e.kind() == std::io::ErrorKind::AddrInUse {
                    StartError::Answered { addr, who: format!("the port is in use ({e})") }
                } else {
                    io(&format!("binding {addr}"), e)
                }
            })?;
            port = l.local_addr().map_err(|e| io("reading the bound port", e))?.port();
            listeners.push(l);
        }
        let addrs: Vec<SocketAddr> = listeners.iter().filter_map(|l| l.local_addr().ok()).collect();

        let journal = Journal::open(&cfg.exec_dir.join(JOURNAL_FILE)).map_err(|e| io("opening the journal", e))?;
        log.line(&format!(
            "started: host {}, version {VERSION}, on {}, journal head {}",
            cfg.host.0,
            addrs.iter().map(ToString::to_string).collect::<Vec<_>>().join(" "),
            journal.head().map_or("none".into(), |EventId(n)| n.to_string()),
        ));

        let (cfg_dir, host_id) = (cfg.exec_dir.clone(), cfg.host.clone());
        let hook_addr = addrs.iter().find(|a| a.ip().is_loopback()).or(addrs.first()).copied();
        let shared = Arc::new(Shared {
            cfg,
            token,
            hook_addr,
            log,
            journal: Mutex::new(journal),
            clients: Mutex::new(Default::default()),
            next_client: AtomicU64::new(1),
            stopping: AtomicBool::new(false),
            wake: (Mutex::new(false), Condvar::new()),
            last_beat: Mutex::new(None),
            hooks: Mutex::new(wsp_host::hooks::Fold::new(&cfg_dir, host_id)),
            seats: Mutex::new(Default::default()),
            jobs: Mutex::new(Default::default()),
        });
        let _ = fs::create_dir_all(shared.run_dir());
        seats::adopt(&shared);
        jobs::adopt(&shared);

        let mut threads = Vec::new();
        for l in listeners {
            let s = shared.clone();
            threads.push(std::thread::spawn(move || accept(s, l)));
        }
        let s = shared.clone();
        threads.push(std::thread::spawn(move || beat(s)));
        Ok(Manager { shared, addrs, threads, _lock: lock })
    }

    /// The addresses it listens on, with the port it bound.
    pub fn addrs(&self) -> &[SocketAddr] {
        &self.addrs
    }

    /// Journal an event and send it to every subscriber. The seat operations
    /// journal their own; this is for tests.
    pub fn emit(&self, msg: Message) -> std::io::Result<EventId> {
        self.shared.emit(msg)
    }

    /// The journal's newest id.
    pub fn head(&self) -> Option<EventId> {
        self.shared.journal.lock().unwrap_or_else(|e| e.into_inner()).head()
    }

    /// The seats it runs now, with their epochs.
    pub fn seats(&self) -> Vec<wsp_host_api::SeatEpoch> {
        self.shared.seats.lock().unwrap_or_else(|e| e.into_inner()).epochs()
    }

    /// The jobs it has, running and ended: `(key, pgid, running)`.
    pub fn jobs(&self) -> Vec<(wsp_host_api::JobKey, u32, bool)> {
        jobs::rows(&self.shared)
    }

    /// How many clients have said a good `hello` and are still connected.
    pub fn clients(&self) -> usize {
        self.shared.clients.lock().unwrap_or_else(|e| e.into_inner()).len()
    }

    /// Write a line to the manager's log.
    pub fn note(&self, what: &str) {
        self.shared.log.line(what);
    }

    /// The log's path.
    pub fn log_path(&self) -> PathBuf {
        self.shared.log.path()
    }

    /// Run until the process ends.
    pub fn wait(self) {
        for t in self.threads {
            let _ = t.join();
        }
    }

    /// Stop listening, close every client and the beat, and release the
    /// `<exec_dir>` and the port: it returns once a restart can take both. The journal is left as it is, for the next start, and so
    /// are the seats: compound-sup outlives its manager, and the next start
    /// adopts them. An order still being asked is asked again then. Jobs are
    /// left running too, undetached ones included, and the next start
    /// re-adopts them.
    pub fn stop(self) {
        self.shared.stopping.store(true, Ordering::SeqCst);
        {
            let (lock, cv) = &self.shared.wake;
            *lock.lock().unwrap_or_else(|e| e.into_inner()) = true;
            cv.notify_all();
        }
        for addr in &self.addrs {
            let _ = TcpStream::connect_timeout(addr, Duration::from_millis(200));
        }
        for c in self.shared.clients.lock().unwrap_or_else(|e| e.into_inner()).values() {
            c.closer.shutdown();
        }
        for t in self.threads {
            let _ = t.join();
        }
        // The accept threads have closed the listeners, but a listening
        // socket, like the lock, lives on in a process this one is spawning
        // until its exec closes the copy, and `shutdown` does not stop a
        // listening socket on macOS. A restart in this process would find
        // the port answered by nothing that says hello, so wait until nothing
        // accepts there (briefly: what holds a copy is one exec away).
        let until = Instant::now() + RELEASE_WAIT;
        for addr in &self.addrs {
            while Instant::now() < until && TcpStream::connect_timeout(addr, Duration::from_millis(200)).is_ok() {
                std::thread::sleep(Duration::from_millis(20));
            }
        }
        self.shared.log.line("stopped");
    }
}

/// `<exec_dir>/manager.lock`, taken: dropping it unlocks it, and then closes it.
///
/// The unlock is what lets the lock go. A `flock` belongs to the open file
/// description, not to the fd, and lasts until its last reference closes. A
/// process spawned by any thread of this process while the lock is open (the
/// beat's `ps`, `sysctl` and the rest, a seat's compound-sup, a job, another
/// test's manager) gets a reference in its copy of the fd table, which lives
/// until its exec closes it (`O_CLOEXEC`; `posix_spawn` too). Closing ours
/// alone would leave the lock held for that window, which a loaded machine
/// stretches, and a manager restarted in this process would find it held,
/// with this pid in it (wsp-080). `LOCK_UN` ends the lock for every
/// reference at once.
struct Lock(File);

impl Drop for Lock {
    fn drop(&mut self) {
        let _ = self.0.unlock();
    }
}

/// Take `<exec_dir>/manager.lock`, which one manager holds for its life.
fn lock(dir: &Path) -> Result<Lock, StartError> {
    let path = dir.join(LOCK_FILE);
    let mut f = OpenOptions::new().read(true).write(true).create(true).truncate(false).open(&path)
        .map_err(|e| io("opening the lock", e))?;
    match f.try_lock() {
        Ok(()) => {}
        Err(TryLockError::WouldBlock) => {
            let holder = fs::read_to_string(&path).unwrap_or_default().trim().to_string();
            return Err(StartError::Locked { dir: dir.to_path_buf(), holder });
        }
        Err(TryLockError::Error(e)) => return Err(io("taking the lock", e)),
    }
    let _ = f.set_len(0);
    let _ = f.write_all(format!("{}\n", std::process::id()).as_bytes());
    Ok(Lock(f))
}

/// What answers at `addr`, if anything: dialled, greeted with this manager's
/// own token, and named by what it says back.
fn answers(addr: SocketAddr, token: &Token) -> Option<String> {
    let mut conn = Tcp::dial(addr, Duration::from_millis(500)).ok()?;
    let hello = Frame {
        id: None,
        req: None,
        msg: Message::Hello(Hello::Wsp(WspHello {
            role: Role::Cli,
            protocol: PROTOCOL,
            version: format!("wsp-manager {VERSION} (start check)"),
            last_event: None,
            token: token.clone(),
        })),
    };
    let _ = conn.send(&hello);
    let said = conn.recv(Duration::from_secs(2));
    conn.shutdown();
    Some(match said {
        Ok(Some(Frame { msg: Message::Hello(Hello::Manager(m)), .. })) => {
            format!("a manager (host {}, version {})", m.host.0, m.version)
        }
        Ok(Some(Frame { msg: Message::Refused(r), .. })) => format!("a manager with another token ({:?})", r.why),
        Ok(Some(_)) | Err(wsp_host_api::ConnError::Malformed(_)) => "something that is not a manager".into(),
        Ok(None) | Err(_) => "something that does not answer a hello".into(),
    })
}

fn accept(shared: Arc<Shared>, listener: TcpListener) {
    for stream in listener.incoming() {
        if shared.stopping.load(Ordering::SeqCst) {
            break;
        }
        let Ok(stream) = stream else { continue };
        let s = shared.clone();
        std::thread::spawn(move || serve::serve(s, stream));
    }
}

/// The beat: every period, read the facts once and send them to every client.
fn beat(shared: Arc<Shared>) {
    let host = wsp_host::InProcess::new();
    let mut missing_before: Vec<&'static str> = Vec::new();
    loop {
        if shared.stopping.load(Ordering::SeqCst) {
            return;
        }
        let read = wsp_host::facts::read(&host, &shared.cfg.volumes);
        if read.missing != missing_before {
            if read.missing.is_empty() {
                shared.log.line("every host fact is read again");
            } else {
                shared.log.line(&format!("host facts not read, sent at their quiet value: {}", read.missing.join(", ")));
            }
            missing_before = read.missing.clone();
        }
        let frame =
            Frame { id: None, req: None, msg: Message::Beat(Beat { at: now(), host: Some(read.facts), seats: vec![] }) };
        {
            let clients = shared.clients.lock().unwrap_or_else(|e| e.into_inner());
            for c in clients.values() {
                let _ = c.out.send(frame.clone());
            }
            *shared.last_beat.lock().unwrap_or_else(|e| e.into_inner()) = Some(frame);
        }
        jobs::sweep(&shared);
        let (lock, cv) = &shared.wake;
        let woken = lock.lock().unwrap_or_else(|e| e.into_inner());
        let _ = cv.wait_timeout_while(woken, shared.cfg.beat, |w| !*w);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_loopback_and_the_tailnet_may_be_bound() {
        let tn = |b: u8, rest: &str| format!("100.{b}.{rest}");
        for ok in ["127.0.0.1".to_string(), "::1".to_string(), tn(64, "0.1"), tn(101, "102.103"), tn(127, "255.254"), "fd7a:115c:a1e0::1".to_string()] {
            assert!(allowed(&ok.parse().unwrap()), "{ok}");
        }
        for no in ["0.0.0.0".to_string(), "::".to_string(), format!("192.{}.1.10", 168), "100.128.0.1".to_string(), "100.63.255.255".to_string(), "10.0.0.1".to_string(), "fd00::1".to_string()] {
            assert!(!allowed(&no.parse().unwrap()), "{no}");
        }
    }

    #[test]
    fn tailscale_ip_lines_are_read() {
        let first = format!("100.{}.102.103", 101);
        let out = format!("{first}\nfd7a:115c:a1e0::1234\n");
        assert_eq!(tailnet_addrs(&out), vec![first.parse::<IpAddr>().unwrap(), "fd7a:115c:a1e0::1234".parse().unwrap()]);
        assert!(tailnet_addrs("no tailscale here\n").is_empty());
    }

    #[test]
    fn a_tailnet_address_this_machine_does_not_hold_is_skipped_and_loopback_still_serves() {
        let dir = std::env::temp_dir().join(format!("wsp-manager-unheld-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        let mut cfg = Config::new(&dir, "t");
        let unheld: IpAddr = format!("100.{}.255.254", 127).parse().unwrap();
        cfg.addrs = vec![IpAddr::V4(Ipv4Addr::LOCALHOST), unheld];
        cfg.port = 0;
        let manager = Manager::start(cfg).expect("loopback serves when a tailnet address cannot be bound");
        assert!(manager.addrs().iter().all(|a| a.ip().is_loopback()), "only loopback is bound: {:?}", manager.addrs());
        let log = fs::read_to_string(dir.join(LOG_DIR).join(log::FILE)).unwrap();
        assert!(log.contains("not on the tailnet") && log.contains(&unheld.to_string()), "the skip is logged: {log}");
        manager.stop();
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_wildcard_address_is_refused_before_anything_is_made() {
        let dir = std::env::temp_dir().join(format!("wsp-manager-wild-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        let mut cfg = Config::new(&dir, "t");
        cfg.addrs = vec!["0.0.0.0".parse().unwrap()];
        assert!(matches!(Manager::start(cfg), Err(StartError::Address(_))));
        assert!(!dir.exists());
    }
}
