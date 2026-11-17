//! `host::Tcp`: the [`Host`] over a connection to the host's manager
//! (`docs/agent-manager.md` 3.2 and 3.4, M1's group 4; wsp-025).
//!
//! Where [`InProcess`] runs a seat's operation here, `Tcp` says it to the
//! manager as its 3.2 message and reads the answer:
//!
//! | trait method | message | answer |
//! |---|---|---|
//! | `spawn_session` | `seat.spawn` | `seat.started` / `seat.refused` |
//! | `stop`, `stop_soon` (a seat it started) | `seat.stop` | `seat.stopped` |
//! | `deliver` (a seat the manager runs) | `order.deliver` | `order.delivered`, `order.acked` / `order.refused` |
//! | `read_screen` (a seat it started) | `seat.read` | `seat.screen` |
//! | `can_deliver`, `wire_health` | `hello` | the manager's `hello` |
//! | `run_job` | `job.run` | `job.started`, `job.output`, `job.ended` / `refused` |
//! | `stop_job` | `job.stop` | `job.ended` / `refused` |
//! | `jobs` | `jobs` | `jobs.list` |
//!
//! [`Tcp::seats`] asks `seats`. Everything else stays in-process under both
//! choices, as M1 leaves it: the process table, signals and working
//! directories (tree work's, until M3), the hook files and transcripts (the
//! manager reports them as events, which the daemon folds at group 5), the
//! host facts as their probes' own text (the beat carries `HostFacts`, not the
//! text), headless seats (no compound-sup), typing at a renderer, and any seat
//! the manager does not run until it adopts it (wsp-029: at the manager's start,
//! from the in-process host's run directory).
//! `Tcp` itself reaches no OS: its in-process half is [`InProcess`].
//!
//! **One connection per CLI call** (3.4): opened at the first message, kept
//! for the process, and reopened only if the manager closed it.
//!
//! **Heard or silent.** A manager that does not answer `hello` within
//! [`SILENT`] is [`HostError::Silent`], "host silent", never an empty answer:
//! the rule place.rs keeps for a census (a backend that did not answer is not
//! a backend with no seats).
//!
//! **Choosing** is `WSP_HOST=inproc|manager` ([`Choice`], [`chosen`]); the
//! default is `manager` since wsp-029, and `WSP_HOST=inproc` is 6.2's rollback.
//! Tree jobs are chosen apart, by `WSP_JOBS=inproc|manager` ([`JobsChoice`],
//! [`jobs_chosen`]; `inproc` until M3's callers have moved), so they roll back
//! on their own.
//!
//! **A job** (M3, wsp-051) runs on a connection of its own: it subscribes
//! from the journal's head, then sends `job.run`, so the connection owns the
//! job (its `job.output` comes there, and an undetached job ends with it) and
//! `job.started` and `job.ended` are heard as they happen, never by asking
//! `jobs` (wsp-037). A dropped connection is dialled again and subscribed
//! from the last id it heard, and the same `job.run` is sent again, its key
//! unchanged, so the manager hands the job over rather than running it twice.
//!
//! [`Subscription`] is the daemon's half: `subscribe {from}` with the last
//! event id it heard of this host, kept in `<state>/manager/<host>.last`, and
//! the same again after a dropped connection. [`Subscription::at_head`] is a
//! CLI call's: a spawn under `manager` sends `seat.spawn` with its first order
//! ([`Tcp::spawn_ordered`]) and hears the order's `order.acked` on one, never
//! polling the seat's state or reading its screen (4.3).

use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::io::BufRead;
use std::net::SocketAddr;
use std::path::{Path, PathBuf};
use std::sync::{Mutex, OnceLock};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use serde_json::Value;
use wsp_host_api::{
    AgentSpec, At, Beat, ConnError, Connection, Epoch, EventId, Frame, Gate, Hello, JobEnded, JobKey, JobOutput, JobRow,
    JobRun, JobStop, Jobs, ManagerHello, Message, HostFacts, OrderDeliver, OrderFail, OrderId, Refusal as Why, ReqId, Role, SeatId, SeatRead, SeatRow, SeatSpawn,
    SeatState, SeatStop, Seats, SeatsList, Subscribe, Token, TurnEdge, Workdir, WspHello, BEAT_SECS, MISSED_BEATS, PROTOCOL,
};

use crate::host::{Ended, Forked, Headless, Host, InProcess, JobError, Session, Spawn, Stop, JOB_GRACE};
/// What a `seats` reply carries, for a caller that reads it (`wip`).
pub use wsp_host_api::{DiskFree, HostFacts as Facts, SeatRow as Row};
use crate::manager::{self, Local};
use crate::place::{Census, Delivery, FirstOrder, Heard, Refusal, Result, Seat, State, Up};

/// How long a manager has to answer `hello` before it is silent.
pub const SILENT: Duration = Duration::from_secs(1);
/// How long a spawn may take: compound-sup's handshake, and the manager's fork.
pub const SPAWN_WAIT: Duration = Duration::from_secs(30);
/// What a request's own answer may take past its stated wait.
const MARGIN: Duration = Duration::from_secs(5);
/// The environment variable that chooses the host.
pub const ENV: &str = "WSP_HOST";
/// The environment variable that chooses the host tree jobs run on.
pub const JOBS_ENV: &str = "WSP_JOBS";

// ---------------------------------------------------------------- errors

/// Why the manager was not asked, or did not answer.
#[derive(Debug, Clone, PartialEq)]
pub enum HostError {
    /// Nothing answered `hello` within [`SILENT`], or the connection closed
    /// without an answer: "host silent", never an empty census.
    Silent { addr: SocketAddr, why: String },
    /// No local machine record to dial (`wsp install` writes it, wsp-024).
    NoRecord(PathBuf),
    /// It answered, and refused: a token that is not its token, the protocol.
    Refused { why: Why, detail: String },
    /// `WSP_HOST` names neither choice.
    Choice(String),
    /// `WSP_JOBS` names neither choice.
    JobsChoice(String),
}

impl std::fmt::Display for HostError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            HostError::Silent { addr, why } => write!(
                f,
                "host silent: the manager at {addr} did not answer within {}s ({why}) — `wsp install --daemon` loads it, or {ENV}=inproc runs the host in this process",
                SILENT.as_secs()
            ),
            HostError::NoRecord(at) => write!(
                f,
                "{ENV}=manager and no manager record at {} — `wsp install` writes it, or {ENV}=inproc runs the host in this process",
                crate::util::contract(at)
            ),
            HostError::Refused { why, detail } => write!(f, "the manager refused wsp's hello: {why:?} {detail}"),
            HostError::Choice(v) => write!(f, "{ENV}={v} is neither `inproc` nor `manager`"),
            HostError::JobsChoice(v) => write!(f, "{JOBS_ENV}={v} is neither `inproc` nor `manager`"),
        }
    }
}

impl std::error::Error for HostError {}

impl From<HostError> for Refusal {
    fn from(e: HostError) -> Refusal {
        Refusal::Unreachable(e.to_string())
    }
}

// ---------------------------------------------------------------- the choice

/// `WSP_HOST`: which implementation of [`Host`] this process asks.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Choice {
    /// [`crate::host::LOCAL`]: the host run in this process, as at M0; 6.2's
    /// rollback. It adopts a seat the manager started by the record's socket
    /// and pid, as one of its own.
    Inproc,
    /// [`Tcp`] to the manager the local machine record names: the default
    /// since wsp-029.
    #[default]
    Manager,
}

impl Choice {
    /// `None` and the empty string are the default, [`Choice::Manager`].
    pub fn parse(value: Option<&str>) -> std::result::Result<Choice, HostError> {
        match value.map(str::trim) {
            None | Some("") | Some("manager") => Ok(Choice::Manager),
            Some("inproc") => Ok(Choice::Inproc),
            Some(other) => Err(HostError::Choice(other.to_string())),
        }
    }

    /// What `$WSP_HOST` says.
    pub fn from_env() -> std::result::Result<Choice, HostError> {
        Choice::parse(std::env::var(ENV).ok().as_deref())
    }
}

/// The host `choice` names, for this machine's `state` dir: the in-process
/// host, or a [`Tcp`] that has already heard the manager's `hello` (so a
/// manager that is not there is [`HostError::Silent`] here, before any seat is
/// asked anything). The `Tcp` is made once per process: one connection per
/// CLI call.
pub fn choose(choice: Choice, state: &Path, version: &str) -> std::result::Result<&'static dyn Host, HostError> {
    static MANAGER: OnceLock<Tcp> = OnceLock::new();
    match choice {
        Choice::Inproc => Ok(&crate::host::LOCAL),
        Choice::Manager => {
            if let Some(tcp) = MANAGER.get() {
                return Ok(tcp);
            }
            let tcp = Tcp::from_record(state, Role::Cli, version)?;
            tcp.hello()?;
            Ok(MANAGER.get_or_init(|| tcp))
        }
    }
}

/// [`choose`] by `$WSP_HOST`.
pub fn chosen(state: &Path, version: &str) -> std::result::Result<&'static dyn Host, HostError> {
    choose(Choice::from_env()?, state, version)
}

/// `WSP_JOBS`: which host tree jobs run on ([`Host::run_job`]). Independent
/// of `WSP_HOST`, so jobs roll back on their own (6.2's M3 rollback).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum JobsChoice {
    /// `wsp-host`'s runner in this process: the default until M3's callers
    /// have moved, and the rollback after.
    #[default]
    Inproc,
    /// `job.run` to the manager the local machine record names. A manager that
    /// does not answer is "host silent", and the job is not run here instead.
    Manager,
}

impl JobsChoice {
    /// `None` and the empty string are the default, [`JobsChoice::Inproc`].
    pub fn parse(value: Option<&str>) -> std::result::Result<JobsChoice, HostError> {
        match value.map(str::trim) {
            None | Some("") | Some("inproc") => Ok(JobsChoice::Inproc),
            Some("manager") => Ok(JobsChoice::Manager),
            Some(other) => Err(HostError::JobsChoice(other.to_string())),
        }
    }

    /// What `$WSP_JOBS` says.
    pub fn from_env() -> std::result::Result<JobsChoice, HostError> {
        JobsChoice::parse(std::env::var(JOBS_ENV).ok().as_deref())
    }
}

/// The host `choice` runs jobs on: the in-process runner, its files under
/// `<state>/inproc`; or the manager's [`Tcp`], the one [`choose`] makes (so a
/// silent manager is [`HostError::Silent`] here, and nothing runs).
pub fn choose_jobs(choice: JobsChoice, state: &Path, version: &str) -> std::result::Result<&'static dyn Host, HostError> {
    static INPROC: OnceLock<InProcess> = OnceLock::new();
    match choice {
        JobsChoice::Inproc => Ok(INPROC.get_or_init(|| InProcess::new().with_jobs(&state.join("inproc"), &crate::util::home()))),
        JobsChoice::Manager => choose(Choice::Manager, state, version),
    }
}

/// [`choose_jobs`] by `$WSP_JOBS`, read once per process as the `Tcp` is
/// made once: a process's jobs do not change host under it.
pub fn jobs_chosen(state: &Path, version: &str) -> std::result::Result<&'static dyn Host, HostError> {
    static READ: OnceLock<std::result::Result<JobsChoice, HostError>> = OnceLock::new();
    choose_jobs(READ.get_or_init(JobsChoice::from_env).clone()?, state, version)
}

// ---------------------------------------------------------------- the census

/// What `wip` reads under `WSP_HOST=manager` (`docs/agent-manager.md` 4.3):
/// one `seats` request to each host in the machine records, in parallel.
#[derive(Debug, Clone)]
pub struct Reading {
    /// Each host's answer is [`Census::heard`]; a host that did not answer
    /// within [`SILENT`] is [`Census::silent`], never an empty answer.
    pub census: Census,
    /// The rows as the managers sent them, by seat: rss and shells.
    pub rows: BTreeMap<String, SeatRow>,
    /// Each answering host's facts, from the same reply, by the census's
    /// name for it (`""` is this machine).
    pub facts: BTreeMap<String, HostFacts>,
}

impl Reading {
    /// Whether any host answered.
    pub fn heard(&self) -> bool {
        self.census.answers().next().is_some()
    }
}

/// A manager's word for a seat, as the census's [`State`].
pub fn state_of(s: SeatState) -> State {
    match s {
        SeatState::Starting => State::Starting,
        SeatState::Idle => State::Idle,
        SeatState::Working => State::Working,
        // Being ended: its agent is going, and no order reaches it.
        SeatState::Stopping => State::Gone,
    }
}

/// One `seats` to every manager the machine records under `state` name, in
/// parallel, each heard or silent within [`SILENT`] of the start: the dial,
/// the `hello` and the reply together. Today the records hold this machine's
/// manager alone (`<state>/manager/local.json`, wsp-024), filed as `""`.
///
/// Reaches no OS here: a seat's label, directory and agent are read from
/// compound's record files, and its state, rss and shells are the manager's.
pub fn census(state: &Path, version: &str) -> Reading {
    let hosts: Vec<(String, std::result::Result<Tcp, HostError>)> = vec![(String::new(), Tcp::from_record(state, Role::Cli, version))];
    let (tx, rx) = std::sync::mpsc::channel();
    let start = Instant::now();
    let mut waiting = BTreeSet::new();
    for (name, tcp) in hosts {
        waiting.insert(name.clone());
        let tx = tx.clone();
        std::thread::spawn(move || {
            let said = tcp.and_then(|t| t.seats_within(SILENT));
            let _ = tx.send((name, said));
        });
    }
    drop(tx);
    let mut said: Vec<(String, std::result::Result<SeatsList, HostError>)> = Vec::new();
    while !waiting.is_empty() {
        let left = SILENT.saturating_sub(start.elapsed());
        match rx.recv_timeout(left) {
            Ok((name, s)) => {
                waiting.remove(&name);
                said.push((name, s));
            }
            Err(_) => break,
        }
    }
    let silent = |why: String| Refusal::Unreachable(format!("host silent: {why}"));
    let compound = crate::place_compound::Compound::new();
    let mut census: Option<Census> = None;
    let mut rows = BTreeMap::new();
    let mut facts = BTreeMap::new();
    let mut fold = |c: Census| census = Some(match census.take() { Some(all) => all.and(c), None => c });
    for (name, s) in said {
        match s {
            Ok(list) => {
                let seats = list.seats.iter().map(|r| compound.seated_as(&Seat::new(&r.seat.0), state_of(r.state))).collect();
                fold(Census::heard(&name, seats));
                rows.extend(list.seats.into_iter().map(|r| (r.seat.0.clone(), r)));
                if let Some(f) = list.host {
                    facts.insert(name, f);
                }
            }
            Err(e) => fold(Census::silent(&name, Refusal::from(e))),
        }
    }
    for name in waiting {
        fold(Census::silent(&name, silent(format!("no answer to `seats` within {}s", SILENT.as_secs()))));
    }
    Reading { census: census.unwrap_or_else(|| Census::silent("", silent("no machine record names a manager".into()))), rows, facts }
}

// ---------------------------------------------------------------- the connection

/// Dial, say `hello`, and read the manager's within [`SILENT`].
fn open(addr: SocketAddr, token: &Token, role: Role, version: &str, last: Option<EventId>) -> std::result::Result<(wsp_host::Tcp, ManagerHello), HostError> {
    let silent = |why: String| HostError::Silent { addr, why };
    let mut conn = wsp_host::Tcp::dial(addr, SILENT).map_err(|e| silent(format!("nothing accepts there: {e:?}")))?;
    let hello = Frame {
        id: None,
        req: None,
        msg: Message::Hello(Hello::Wsp(WspHello {
            role,
            protocol: PROTOCOL,
            version: version.to_string(),
            last_event: last,
            token: token.clone(),
        })),
    };
    conn.send(&hello).map_err(|e| silent(format!("it did not take a hello: {e:?}")))?;
    let deadline = Instant::now() + SILENT;
    loop {
        let left = deadline.saturating_duration_since(Instant::now());
        if left.is_zero() {
            conn.shutdown();
            return Err(silent("no hello came back".into()));
        }
        match conn.recv(left) {
            Ok(Some(Frame { msg: Message::Hello(Hello::Manager(m)), .. })) => return Ok((conn, m)),
            Ok(Some(Frame { msg: Message::Refused(r), .. })) => {
                conn.shutdown();
                return Err(HostError::Refused { why: r.why, detail: r.detail });
            }
            Ok(Some(_)) => continue,
            Ok(None) => {
                conn.shutdown();
                return Err(silent("no hello came back".into()));
            }
            Err(ConnError::Closed) => return Err(silent("it closed the connection on a hello".into())),
            Err(e) => {
                conn.shutdown();
                return Err(silent(format!("what answered is not a manager: {e:?}")));
            }
        }
    }
}

fn beat_frame() -> Frame {
    Frame { id: None, req: None, msg: Message::Beat(Beat { at: now(), host: None, seats: vec![] }) }
}

fn now() -> At {
    At(SystemTime::now().duration_since(UNIX_EPOCH).map_or(0, |d| d.as_secs()))
}

/// The connection and what its `hello` said.
struct Live {
    conn: wsp_host::Tcp,
    hello: ManagerHello,
    sent: Instant,
}

/// A seat the manager started for this process, by what the trait's callers
/// hold of it: its session pid (the group a stop names) and its render socket.
#[derive(Debug, Clone)]
struct Known {
    epoch: Epoch,
    pid: u32,
    render: Option<PathBuf>,
    /// The journal id of this start's `seat.started`, as the spawn's answer
    /// carried it: what [`Host::await_up`] subscribes after, so it hears this
    /// start's first turn or its stop and nothing before (wsp-037). `None`
    /// where the answer carried none (a journal that could not be written).
    started: Option<EventId>,
}

/// The [`Host`] over a connection to the host's manager.
pub struct Tcp {
    addr: SocketAddr,
    token: Token,
    role: Role,
    version: String,
    /// What M1 does not move, and seats the manager does not run.
    local: InProcess,
    live: Mutex<Option<Live>>,
    next_req: Mutex<u64>,
    /// The seats the manager started for this process.
    known: Mutex<BTreeMap<String, Known>>,
    /// The first orders sent with a `seat.spawn` whose answer is not heard
    /// yet, by order id: the seat, and the subscription that will hear it.
    waiting: Mutex<BTreeMap<String, (String, Subscription)>>,
}

impl Tcp {
    /// A `Tcp` to `addr` with `token`; nothing is dialled until the first message.
    pub fn new(addr: SocketAddr, token: Token, role: Role, version: &str) -> Tcp {
        Tcp::with_local(addr, token, role, version, InProcess::new())
    }

    /// The same, with the in-process half handed in (a test's `compound-sup`).
    pub fn with_local(addr: SocketAddr, token: Token, role: Role, version: &str, local: InProcess) -> Tcp {
        Tcp {
            addr,
            token,
            role,
            version: version.to_string(),
            local,
            live: Mutex::new(None),
            next_req: Mutex::new(0),
            known: Mutex::new(BTreeMap::new()),
            waiting: Mutex::new(BTreeMap::new()),
        }
    }

    /// The manager the local machine record under `state` names.
    pub fn from_record(state: &Path, role: Role, version: &str) -> std::result::Result<Tcp, HostError> {
        let (local, token): (Local, Token) =
            manager::read_record(state).ok_or_else(|| HostError::NoRecord(manager::record_path(state)))?;
        Ok(Tcp::new(local.socket(), token, role, version))
    }

    pub fn addr(&self) -> SocketAddr {
        self.addr
    }

    /// The manager's `hello`, from the connection this process holds (opened
    /// if it is not yet).
    pub fn hello(&self) -> std::result::Result<ManagerHello, HostError> {
        let mut live = self.live.lock().unwrap_or_else(|e| e.into_inner());
        Ok(self.connected(&mut live)?.hello.clone())
    }

    fn connected<'a>(&self, live: &'a mut Option<Live>) -> std::result::Result<&'a mut Live, HostError> {
        if live.is_none() {
            let (conn, hello) = open(self.addr, &self.token, self.role, &self.version, None)?;
            *live = Some(Live { conn, hello, sent: Instant::now() });
        }
        Ok(live.as_mut().expect("just opened"))
    }

    /// Send `msg` under a fresh `req` and collect the answers carrying it until
    /// `done` says one ends the exchange, for at most `wait`. A connection the
    /// manager closed is reopened once, before anything was sent on it.
    fn ask(&self, msg: Message, wait: Duration, done: impl Fn(&Message) -> bool) -> std::result::Result<Vec<Message>, HostError> {
        Ok(self.ask_frames(msg, wait, done)?.into_iter().map(|f| f.msg).collect())
    }

    /// [`Tcp::ask`], keeping each answer's frame: a journalled answer carries
    /// its event id.
    fn ask_frames(&self, msg: Message, wait: Duration, done: impl Fn(&Message) -> bool) -> std::result::Result<Vec<Frame>, HostError> {
        let req = {
            let mut n = self.next_req.lock().unwrap_or_else(|e| e.into_inner());
            *n += 1;
            ReqId(*n)
        };
        let frame = Frame { id: None, req: Some(req), msg };
        let mut guard = self.live.lock().unwrap_or_else(|e| e.into_inner());
        let sent = match self.connected(&mut guard)?.conn.send(&frame) {
            Ok(()) => Ok(()),
            Err(_) => {
                // Closed under us (the manager restarted): once more, fresh.
                *guard = None;
                self.connected(&mut guard)?.conn.send(&frame)
            }
        };
        let silent = |why: String| HostError::Silent { addr: self.addr, why };
        sent.map_err(|e| silent(format!("the request was not taken: {e:?}")))?;
        let live = guard.as_mut().expect("connected above");
        live.sent = Instant::now();
        let deadline = Instant::now() + wait;
        let mut said = Vec::new();
        loop {
            let left = deadline.saturating_duration_since(Instant::now());
            if left.is_zero() {
                return Ok(said);
            }
            // Our own beat, so a long wait is not dropped as a silent client.
            if live.sent.elapsed() >= Duration::from_secs(BEAT_SECS) {
                let _ = live.conn.send(&beat_frame());
                live.sent = Instant::now();
            }
            match live.conn.recv(left.min(Duration::from_secs(BEAT_SECS))) {
                Ok(Some(f)) if f.req == Some(req) => {
                    let end = done(&f.msg) || matches!(f.msg, Message::Refused(_));
                    said.push(f);
                    if end {
                        return Ok(said);
                    }
                }
                Ok(_) => {}
                Err(e) => {
                    *guard = None;
                    return match said.is_empty() {
                        true => Err(silent(format!("the connection dropped before an answer: {e:?}"))),
                        false => Ok(said),
                    };
                }
            }
        }
    }

    /// The manager's epoch for `seat`: one it started for this process, else
    /// its `hello`'s list. `None`: the manager does not run it.
    fn epoch_of(&self, seat: &str) -> std::result::Result<Option<Epoch>, HostError> {
        if let Some(k) = self.known.lock().unwrap_or_else(|e| e.into_inner()).get(seat) {
            return Ok(Some(k.epoch));
        }
        Ok(self.hello()?.seats.into_iter().find(|s| s.seat.0 == seat).map(|s| s.epoch))
    }

    fn known_by(&self, f: impl Fn(&Known) -> bool) -> Option<(String, Known)> {
        self.known.lock().unwrap_or_else(|e| e.into_inner()).iter().find(|(_, k)| f(k)).map(|(s, k)| (s.clone(), k.clone()))
    }

    /// `seats`: what the manager runs, per seat.
    pub fn seats(&self) -> std::result::Result<Vec<SeatRow>, HostError> {
        self.seats_within(SPAWN_WAIT).map(|l| l.seats)
    }

    /// `seats`, with the host facts the reply carries, answered within `wait`
    /// or [`HostError::Silent`].
    pub fn seats_within(&self, wait: Duration) -> std::result::Result<SeatsList, HostError> {
        let said = self.ask(Message::Seats(Seats {}), wait, |m| matches!(m, Message::SeatsList(_)))?;
        match said.into_iter().next() {
            Some(Message::SeatsList(l)) => Ok(l),
            Some(Message::Refused(r)) => Err(HostError::Refused { why: r.why, detail: r.detail }),
            _ => Err(HostError::Silent { addr: self.addr, why: format!("no answer to `seats` within {}s", wait.as_secs()) }),
        }
    }

    /// `seat.stop` of `seat` at `epoch`, and its `seat.stopped` waited for up
    /// to `wait` (nothing waited for when zero): what the manager ended.
    fn stop_seat(&self, seat: &str, epoch: Epoch, grace: Duration, wait: Duration) -> Result<Ended> {
        let msg = Message::SeatStop(SeatStop {
            seat: SeatId(seat.to_string()),
            epoch,
            grace_secs: u32::try_from(grace.as_secs() + u64::from(grace.subsec_nanos() > 0)).unwrap_or(u32::MAX),
        });
        let said = self.ask(msg, wait, |m| matches!(m, Message::SeatStopped(_)))?;
        match said.into_iter().last() {
            Some(Message::SeatStopped(s)) => {
                self.known.lock().unwrap_or_else(|e| e.into_inner()).remove(seat);
                Ok(Ended { pids: s.ended, exit: s.exit })
            }
            Some(Message::Refused(r)) => Err(Refusal::Backend(format!("the manager refused to stop {seat}: {:?} {}", r.why, r.detail))),
            _ if wait.is_zero() => Ok(Ended::default()),
            _ => Err(Refusal::Unreachable(format!(
                "host silent: no seat.stopped for {seat} within {}s from the manager at {}",
                wait.as_secs(),
                self.addr
            ))),
        }
    }

    /// `seat.spawn`, with `first` for the manager to deliver once the seat has
    /// started, and its `seat.started` or `seat.refused` within `wait`. The
    /// answer is the request's own: `seat.refused` goes only to the asker, never
    /// into the journal, so a subscription would not hear it.
    fn spawn_with(&self, spawn: &Spawn<'_>, first: Option<FirstOrder>, wait: Duration) -> Result<Session> {
        let msg = Message::SeatSpawn(SeatSpawn {
            // The caller's (row, attempt), built once per spawn: `ask`'s one
            // reopen resends this same message, so the key cannot change.
            key: spawn.key.clone(),
            seat: SeatId(spawn.seat.to_string()),
            agent: AgentSpec { argv: argv_of(spawn), env: vec![], kind: spawn.kind.clone().unwrap_or_default(), model: None },
            workdir: Workdir::Cwd(spawn.cwd.clone()),
            gate: Gate::None,
            first_order: first,
        });
        let said = self.ask_frames(msg, wait, |m| matches!(m, Message::SeatStarted(_) | Message::SeatRefused(_)))?;
        let Some(Frame { id, msg, .. }) = said.into_iter().last() else {
            return Err(Refusal::Backend(format!("no seat.started for {} within {}s", spawn.seat, wait.as_secs())));
        };
        match msg {
            Message::SeatStarted(s) => {
                let socket = s.sockets.first().map(PathBuf::from).unwrap_or_default();
                let render = s.sockets.get(1).map(PathBuf::from);
                let known = Known { epoch: s.epoch, pid: s.pid, render: render.clone(), started: id };
                self.known.lock().unwrap_or_else(|e| e.into_inner()).insert(spawn.seat.to_string(), known);
                Ok(Session { pid: s.pid, socket, render })
            }
            Message::SeatRefused(r) => Err(Refusal::Backend(format!("the manager refused seat {}: {:?} {}", r.seat.0, r.why, r.detail))),
            Message::Refused(r) => Err(Refusal::Backend(format!("the manager refused the spawn: {:?} {}", r.why, r.detail))),
            _ => Err(Refusal::Backend(format!("no seat.started for {} within {}s", spawn.seat, wait.as_secs()))),
        }
    }
}

// ---------------------------------------------------------------- jobs

/// How long a job's dropped connection is dialled again before the manager is
/// silent: as long as a subscription bears a manager it does not hear.
fn rejoin_patience() -> Duration {
    Duration::from_secs(BEAT_SECS) * MISSED_BEATS
}

impl Tcp {
    fn fresh_req(&self) -> ReqId {
        let mut n = self.next_req.lock().unwrap_or_else(|e| e.into_inner());
        *n += 1;
        ReqId(*n)
    }

    /// A connection for one job: dialled, subscribed from `from` (the
    /// journal's head on the first), and the job's `job.run` sent on it. The
    /// id it subscribed from comes back with it.
    fn job_conn(&self, from: Option<EventId>, run: &Frame) -> std::result::Result<(wsp_host::Tcp, Option<EventId>), HostError> {
        let (mut conn, hello) = open(self.addr, &self.token, self.role, &self.version, from)?;
        let from = from.or(hello.journal_head);
        let silent = |why: String| HostError::Silent { addr: self.addr, why };
        let sub = Frame { id: None, req: None, msg: Message::Subscribe(Subscribe { from }) };
        conn.send(&sub).map_err(|e| silent(format!("it did not take subscribe: {e:?}")))?;
        conn.send(run).map_err(|e| silent(format!("it did not take job.run: {e:?}")))?;
        Ok((conn, from))
    }

    /// [`Tcp::job_conn`] again after a drop, from the last id heard, dialled
    /// until [`rejoin_patience`] is out (a manager restarting is a moment
    /// with nothing listening).
    fn job_rejoin(&self, from: Option<EventId>, run: &Frame) -> std::result::Result<(wsp_host::Tcp, Option<EventId>), HostError> {
        let start = Instant::now();
        loop {
            match self.job_conn(from, run) {
                Err(HostError::Silent { .. }) if start.elapsed() < rejoin_patience() => std::thread::sleep(Duration::from_millis(200)),
                other => return other,
            }
        }
    }
}

/// Read `sub` for `order`'s answer up to `wait`: `order.acked`, `order.refused`
/// (worded as the in-process host words compound-sup's reason), or the seat
/// stopping first. Everything else on the subscription is someone else's.
fn heard_of(sub: &mut Subscription, seat: &str, order: &OrderId, wait: Duration) -> Result<Heard> {
    let deadline = Instant::now() + wait;
    loop {
        let left = deadline.saturating_duration_since(Instant::now());
        if left.is_zero() {
            return Ok(Heard::Unanswered);
        }
        let Some(f) = sub.next(left)? else { continue };
        match f.msg {
            Message::OrderAcked(a) if a.order == *order => return Ok(Heard::Acked),
            Message::OrderRefused(r) if r.order == *order => return Ok(Heard::Refused(format!("{seat}: {}", fail_word(r.why)))),
            Message::SeatStopped(s) if s.seat.0 == seat => {
                return Ok(Heard::Refused(format!("{seat} stopped before its work order was answered")))
            }
            _ => {}
        }
    }
}

/// compound-sup's failure word for an `order.refused`, as `read_deliver`
/// words it in-process: `"busy"`, `"no-composer"`, ….
fn fail_word(why: OrderFail) -> String {
    serde_json::to_value(why).ok().and_then(|v| v.as_str().map(|s| s.replace('_', "-"))).unwrap_or_else(|| "unknown".into())
}

/// The spawn's environment, carried as an `env` prefix on the argv: 3.2's
/// agent spec carries env *names*, which the manager fills from its own
/// environment, and a seat's values (its row, its store) are not in that.
fn argv_of(spawn: &Spawn<'_>) -> Vec<String> {
    let mut argv = Vec::new();
    if !spawn.env.is_empty() {
        argv.push("env".to_string());
        for (k, _) in spawn.env.iter().filter(|(_, v)| v.is_empty()) {
            argv.extend(["-u".to_string(), k.clone()]);
        }
        argv.push("--".to_string());
        argv.extend(spawn.env.iter().filter(|(_, v)| !v.is_empty()).map(|(k, v)| format!("{k}={v}")));
    }
    argv.push(spawn.program.to_string());
    argv.extend(spawn.args.iter().cloned());
    argv
}

impl Host for Tcp {
    // ---- seat.spawn / seat.started
    fn spawn_session(&self, spawn: &Spawn<'_>) -> Result<Session> {
        // On the one connection, and nothing held open after it: the start's
        // `seat.started` carries its journal id, and `await_up` subscribes
        // after that id, so the start's first turn or its stop is replayed to
        // it however late it asks (wsp-037). A spawn no `await_up` follows (a
        // bare shell `open` starts, a start that errs after) leaves no watch.
        self.spawn_with(spawn, None, SPAWN_WAIT)
    }
    fn await_up(&self, seat: &str, by_turn: bool, wait: Duration) -> Result<Up> {
        // `seat.started` was the spawn's own answer: that is the seat up, and
        // where readiness is not a turn there is nothing more to hear.
        if !by_turn {
            return Ok(Up::Ready);
        }
        let known = self.known.lock().unwrap_or_else(|e| e.into_inner()).get(seat).map(|k| (k.epoch, k.started));
        // Not started by this process, or its `seat.started` carried no id to
        // replay after: there is nothing to hear it by but that answer.
        let Some((epoch, Some(started))) = known else { return Ok(Up::Ready) };
        let mut sub = Subscription::after(self.addr, self.token.clone(), self.role, &self.version, started)?;
        let ours = |s: &SeatId, e: Epoch| s.0 == seat && e == epoch;
        let deadline = Instant::now() + wait;
        let up = loop {
            let left = deadline.saturating_duration_since(Instant::now());
            if left.is_zero() {
                break Up::Unready;
            }
            let Some(f) = sub.next(left)? else { continue };
            match f.msg {
                Message::Turn(t) if t.edge == TurnEdge::Began && ours(&t.seat, t.epoch) => break Up::Ready,
                Message::SeatStopped(s) if ours(&s.seat, s.epoch) => break Up::Stopped,
                _ => {}
            }
        };
        sub.close();
        Ok(up)
    }
    fn hears_orders(&self) -> bool {
        true
    }
    fn spawn_ordered(&self, spawn: &Spawn<'_>, order: &FirstOrder, wait: Duration) -> Result<(Session, Heard)> {
        let start = Instant::now();
        // Subscribed before the spawn is sent, from the journal's head as it
        // stands: the order's answer cannot come before the watch on it.
        let sub = Subscription::at_head(self.addr, self.token.clone(), self.role, &self.version)?;
        let session = self.spawn_with(spawn, Some(order.clone()), wait)?;
        self.waiting.lock().unwrap_or_else(|e| e.into_inner()).insert(order.order.0.clone(), (spawn.seat.to_string(), sub));
        // `seat.started` is heard: the seat runs, whatever the subscription
        // does next. One that cannot be held (the manager went while its ack
        // was awaited) leaves the order unanswered, never the start undone:
        // an error here would drop a running seat's session and its record.
        // The watch is kept, so a later `await_order` asks again from the last
        // id heard and says why if it still cannot.
        let heard = self.await_order(&order.order, wait.saturating_sub(start.elapsed())).unwrap_or(Heard::Unanswered);
        Ok((session, heard))
    }
    fn await_order(&self, order: &OrderId, wait: Duration) -> Result<Heard> {
        let Some((seat, mut sub)) = self.waiting.lock().unwrap_or_else(|e| e.into_inner()).remove(&order.0) else {
            return Err(Refusal::Backend(format!("order {} was not sent by this process's seat.spawn", order.0)));
        };
        let heard = match heard_of(&mut sub, &seat, order, wait) {
            Ok(heard) => heard,
            Err(e) => {
                // Not heard, not lost: the order is still the seat's, and a
                // manager that comes back answers it from the journal.
                self.waiting.lock().unwrap_or_else(|e| e.into_inner()).insert(order.0.clone(), (seat, sub));
                return Err(e);
            }
        };
        match heard {
            Heard::Unanswered => {
                self.waiting.lock().unwrap_or_else(|e| e.into_inner()).insert(order.0.clone(), (seat, sub));
            }
            _ => sub.close(),
        }
        Ok(heard)
    }
    fn spawn_headless(&self, spawn: &Headless<'_>) -> Result<Forked> {
        self.local.spawn_headless(spawn)
    }

    // ---- seat.stop / seat.stopped
    fn stop(&self, stop: Stop<'_>) {
        match self.known_by(|k| k.pid == stop.group) {
            Some((seat, k)) => {
                let _ = self.stop_seat(&seat, k.epoch, stop.grace, stop.grace + MARGIN);
            }
            None => self.local.stop(stop),
        }
    }
    /// By seat, so a `despawn` in a process that started nothing finds the seat
    /// in the manager's own list: the manager's pids, or the plain reason it
    /// could not say. A seat the manager answers it does not run is ended
    /// in-process (wsp-040): a terminal this process opened and the manager
    /// refused to start in, or a seat started before the manager, is still a
    /// group here, and refusing to end it kept its claim with nothing on it.
    fn end(&self, seat: &str, stop: Stop<'_>) -> Result<Ended> {
        match self.epoch_of(seat)? {
            Some(epoch) => self.stop_seat(seat, epoch, stop.grace, stop.grace + MARGIN),
            None => self.local.end(seat, stop),
        }
    }
    fn stop_soon(&self, group: u32) {
        match self.known_by(|k| k.pid == group) {
            Some((seat, k)) => {
                let _ = self.stop_seat(&seat, k.epoch, Duration::from_secs(1), Duration::ZERO);
            }
            None => self.local.stop_soon(group),
        }
    }
    fn clear_socket(&self, socket: &Path, sidecars: &[&str]) {
        self.local.clear_socket(socket, sidecars)
    }
    fn unlink(&self, path: &Path) {
        self.local.unlink(path)
    }

    // ---- seats: the process table stays in-process at M1 (tree work's, M3)
    fn alive(&self, pids: &[u32]) -> BTreeSet<u32> {
        self.local.alive(pids)
    }
    fn tree_of(&self, root: u32) -> Vec<u32> {
        self.local.tree_of(root)
    }
    fn answers(&self, socket: &Path) -> bool {
        self.local.answers(socket)
    }
    fn started(&self, pid: u32) -> std::result::Result<Option<String>, ()> {
        self.local.started(pid)
    }
    fn shells(&self) -> Option<String> {
        self.local.shells()
    }
    fn seat_memory(&self) -> Option<String> {
        self.local.seat_memory()
    }
    fn table(&self, columns: &str, env: bool) -> Option<String> {
        self.local.table(columns, env)
    }
    fn rows_of(&self, pids: &[u32], columns: &str) -> Option<String> {
        self.local.rows_of(pids, columns)
    }
    fn signal(&self, pids: &[u32], sig: &str) -> Option<bool> {
        self.local.signal(pids, sig)
    }
    fn signal_group(&self, group: u32, sig: &str) {
        self.local.signal_group(group, sig)
    }
    fn cwds(&self, pids: Option<&[u32]>) -> Option<String> {
        self.local.cwds(pids)
    }

    // ---- seat.read
    fn read_screen(&self, render: &Path) -> Result<String> {
        let Some((seat, _)) = self.known_by(|k| k.render.as_deref() == Some(render)) else {
            return self.local.read_screen(render);
        };
        let said = self.ask(Message::SeatRead(SeatRead { seat: SeatId(seat.clone()) }), SPAWN_WAIT, |m| matches!(m, Message::SeatScreen(_)))?;
        match said.into_iter().last() {
            Some(Message::SeatScreen(s)) => Ok(s.text),
            Some(Message::Refused(r)) if r.why == Why::NoSeat => Err(Refusal::NoSeat(Seat::new(seat))),
            Some(Message::Refused(r)) => Err(Refusal::Backend(format!("the manager could not read {seat}'s screen: {:?} {}", r.why, r.detail))),
            _ => Err(Refusal::Backend(format!("the manager did not answer {seat}'s seat.read"))),
        }
    }
    fn screen_state(&self, render: &Path, kind: &str) -> Option<State> {
        self.local.screen_state(render, kind)
    }
    fn osc_title(&self, socket: &Path) -> Option<String> {
        self.local.osc_title(socket)
    }

    // ---- order.deliver / order.delivered / order.refused
    fn can_deliver(&self) -> bool {
        self.hello().is_ok() || self.local.can_deliver()
    }
    fn deliver(&self, session: &Path, id: &str, text: &str, wait: Duration, seat: &Seat) -> Result<Delivery> {
        let Some(epoch) = self.epoch_of(seat.as_str())? else {
            // A seat the manager does not run: started in-process.
            return self.local.deliver(session, id, text, wait, seat);
        };
        let msg = Message::OrderDeliver(OrderDeliver {
            order: OrderId(id.to_string()),
            seat: SeatId(seat.as_str().to_string()),
            epoch,
            text: text.to_string(),
            now: false,
        });
        let said = self.ask(msg, wait + MARGIN, |m| matches!(m, Message::OrderAcked(_) | Message::OrderRefused(_)))?;
        match said.last() {
            Some(Message::OrderAcked(_)) => Ok(Delivery::Acked),
            Some(Message::OrderRefused(r)) => match r.why {
                OrderFail::SessionGone | OrderFail::NoSeat => Err(Refusal::NoSeat(seat.clone())),
                why => Err(Refusal::Failed(format!("{seat}: {}", fail_word(why)))),
            },
            Some(Message::Refused(r)) => Err(Refusal::Backend(format!("the manager refused order {id}: {:?} {}", r.why, r.detail))),
            Some(Message::OrderDelivered(_)) => Ok(Delivery::Unconfirmed),
            _ => Err(Refusal::Backend(format!(
                "{seat}: {} after {}s, asked again next tick under the same id",
                crate::host::DELIVERY_PENDING,
                wait.as_secs()
            ))),
        }
    }
    fn type_in(&self, render: &Path, text: &str) -> Result<()> {
        self.local.type_in(render, text)
    }
    fn press_enter(&self, render: &Path) -> Result<()> {
        self.local.press_enter(render)
    }
    fn append_line(&self, file: &Path, line: &str) -> Result<()> {
        self.local.append_line(file, line)
    }

    // ---- seat.said / seat.spent: the files stay; the events are group 5's
    fn said(&self, dir: &Path) -> Value {
        self.local.said(dir)
    }
    fn say(&self, dir: &Path, said: &Value) {
        self.local.say(dir, said)
    }
    fn log_hook(&self, dir: &Path, line: &Value) {
        self.local.log_hook(dir, line)
    }
    fn quiet_since(&self, dir: &Path) -> Option<i64> {
        self.local.quiet_since(dir)
    }
    fn tally(&self, dir: &Path, payload: &Value) {
        self.local.tally(dir, payload)
    }
    fn bank(&self, dir: &Path) {
        self.local.bank(dir)
    }
    fn burn_under(&self, root: &Path) -> Vec<(String, Value)> {
        self.local.burn_under(root)
    }

    // ---- seat.did
    fn transcript_of(&self, session: &str, cwd: &str) -> Option<PathBuf> {
        self.local.transcript_of(session, cwd)
    }
    fn pi_session_of(&self, session: &str, cwd: &str) -> Option<PathBuf> {
        self.local.pi_session_of(session, cwd)
    }
    fn read_transcript(&self, file: &Path) -> Option<Box<dyn BufRead>> {
        self.local.read_transcript(file)
    }
    fn transcript_bytes(&self, file: &Path) -> Option<Vec<u8>> {
        self.local.transcript_bytes(file)
    }
    fn transcript_tail(&self, file: &Path, bytes: u64) -> Option<String> {
        self.local.transcript_tail(file, bytes)
    }
    fn opencode_export(&self, session: &str) -> Option<Vec<u8>> {
        self.local.opencode_export(session)
    }
    fn opencode_newest(&self, session: &str) -> Option<Vec<Value>> {
        self.local.opencode_newest(session)
    }

    // ---- turn
    fn turn_began(&self, dir: &Path, since: i64) -> Option<bool> {
        self.local.turn_began(dir, since)
    }

    // ---- beat: the probes' text stays in-process until `HostFacts` replaces it
    fn disk_free(&self, path: &Path) -> Option<String> {
        self.local.disk_free(path)
    }
    fn disk_use(&self, path: &Path) -> Option<String> {
        self.local.disk_use(path)
    }
    fn load(&self) -> Option<String> {
        self.local.load()
    }
    fn thermal(&self) -> Option<String> {
        self.local.thermal()
    }
    fn memory_pressure(&self) -> Option<String> {
        self.local.memory_pressure()
    }
    fn swap(&self) -> Option<String> {
        self.local.swap()
    }
    fn pageouts(&self) -> Option<String> {
        self.local.pageouts()
    }
    fn idle(&self) -> Option<String> {
        self.local.idle()
    }

    // ---- hello
    // ---- job.run / job.stop / jobs
    fn run_job(&self, run: &JobRun, sink: &mut dyn FnMut(&JobOutput)) -> std::result::Result<JobEnded, JobError> {
        // Built once: a resend after a reopen is this same frame, its key
        // unchanged, so the manager hands the job over and never runs it twice.
        let frame = Frame { id: None, req: Some(self.fresh_req()), msg: Message::JobRun(run.clone()) };
        let req = frame.req;
        let (mut conn, mut last) = self.job_conn(None, &frame).map_err(JobError::Silent)?;
        // The manager ends the job at its timeout and kills it within the
        // grace: past that and a margin, no end is coming.
        let bound = Duration::from_secs(run.timeout_secs).checked_add(JOB_GRACE + MARGIN * 2);
        let deadline = bound.and_then(|b| Instant::now().checked_add(b));
        let patience = rejoin_patience();
        let (mut heard, mut sent) = (Instant::now(), Instant::now());
        // The offset of the last output handed on, so a chunk is handed once.
        let mut handed: Option<u64> = None;
        let mut rejoined = false;
        // A `duplicate` to a resend: the job ended while this was away, and
        // the journal's replay of its `job.ended` may come just after.
        let mut duplicate: Option<(Instant, String)> = None;
        let unheard = |why: String| JobError::Unanswered(format!("job {}: {why}, from the manager at {}", run.key.job, self.addr));
        loop {
            if deadline.is_some_and(|d| Instant::now() >= d) {
                conn.shutdown();
                return Err(unheard(format!("no job.ended within {}s of its timeout", (JOB_GRACE + MARGIN * 2).as_secs())));
            }
            if let Some((at, detail)) = &duplicate {
                if at.elapsed() >= MARGIN {
                    conn.shutdown();
                    return Err(JobError::Refused { why: Why::Duplicate, detail: detail.clone() });
                }
            }
            let mut dropped = heard.elapsed() >= patience;
            if !dropped {
                // Our own beat, so a long job is not dropped as a silent client.
                if sent.elapsed() >= Duration::from_secs(BEAT_SECS) {
                    let _ = conn.send(&beat_frame());
                    sent = Instant::now();
                }
                match conn.recv(Duration::from_secs(1)) {
                    Ok(Some(f)) => {
                        heard = Instant::now();
                        if let Some(id) = f.id {
                            last = last.max(Some(id));
                        }
                        match f.msg {
                            Message::JobEnded(e) if e.key == run.key => {
                                conn.shutdown();
                                return Ok(e);
                            }
                            Message::JobOutput(o) if o.key == run.key => {
                                if handed.is_some_and(|h| o.from <= h) {
                                    continue;
                                }
                                handed = Some(o.from);
                                sink(&o);
                            }
                            Message::Refused(r) if f.req == req => {
                                if r.why == Why::Duplicate && rejoined {
                                    duplicate.get_or_insert((Instant::now(), r.detail));
                                    continue;
                                }
                                conn.shutdown();
                                return Err(JobError::Refused { why: r.why, detail: r.detail });
                            }
                            // `job.started`: it runs, and its end is what is
                            // waited for. Anything else is someone else's.
                            _ => {}
                        }
                    }
                    Ok(None) | Err(ConnError::Malformed(_)) => {}
                    Err(_) => dropped = true,
                }
            }
            if dropped {
                conn.shutdown();
                (conn, last) = self.job_rejoin(last, &frame).map_err(JobError::Silent)?;
                (heard, sent, rejoined) = (Instant::now(), Instant::now(), true);
            }
        }
    }
    fn stop_job(&self, key: &JobKey, wait: Duration) -> std::result::Result<JobEnded, JobError> {
        let msg = Message::JobStop(JobStop { key: key.clone() });
        let said = self.ask(msg, wait, |m| matches!(m, Message::JobEnded(e) if e.key == *key)).map_err(JobError::Silent)?;
        match said.into_iter().last() {
            Some(Message::JobEnded(e)) => Ok(e),
            Some(Message::Refused(r)) => Err(JobError::Refused { why: r.why, detail: r.detail }),
            _ => Err(JobError::Unanswered(format!(
                "no job.ended for {} within {}s of its job.stop, from the manager at {}",
                key.job,
                wait.as_secs(),
                self.addr
            ))),
        }
    }
    fn jobs(&self) -> std::result::Result<Vec<JobRow>, JobError> {
        let said = self.ask(Message::Jobs(Jobs {}), SPAWN_WAIT, |m| matches!(m, Message::JobsList(_))).map_err(JobError::Silent)?;
        match said.into_iter().next() {
            Some(Message::JobsList(l)) => Ok(l.jobs),
            Some(Message::Refused(r)) => Err(JobError::Refused { why: r.why, detail: r.detail }),
            _ => Err(JobError::Silent(HostError::Silent { addr: self.addr, why: format!("no answer to `jobs` within {}s", SPAWN_WAIT.as_secs()) })),
        }
    }

    fn wire_health(&self, problems: &mut Vec<String>, notes: &mut Vec<String>) {
        match self.hello() {
            Ok(m) => {
                notes.push(format!("manager {} answers hello at {} (version {}, protocol {})", m.host.0, self.addr, m.version, m.protocol));
                manager::sup_in_range(manager::installed_sup().as_deref(), m.compound_sup, problems, notes);
            }
            Err(e) => problems.push(e.to_string()),
        }
    }
}

// ---------------------------------------------------------------- the subscription

/// Where the last event id heard of `host` is kept.
pub fn last_path(state: &Path, host: &str) -> PathBuf {
    let safe: String = host.chars().map(|c| if c.is_ascii_alphanumeric() || "-_.".contains(c) { c } else { '_' }).collect();
    state.join("manager").join(format!("{safe}.last"))
}

/// The last event id heard of `host`, if one was kept.
pub fn read_last(state: &Path, host: &str) -> Option<EventId> {
    fs::read_to_string(last_path(state, host)).ok()?.trim().parse().ok().map(EventId)
}

/// One subscription to a manager's events: `subscribe {from}` with the last id
/// heard of this host, the events after it one at a time, and the same again
/// after the connection drops. An event's id is kept once the next is asked
/// for, so one that was handed out and not folded (the caller died) is heard
/// again: at least once, never skipped.
pub struct Subscription {
    addr: SocketAddr,
    token: Token,
    host: String,
    /// Where the last id is kept; `None` for a CLI call's watch, which keeps
    /// its place in memory and never moves the daemon's.
    state: Option<PathBuf>,
    role: Role,
    version: String,
    conn: Option<(wsp_host::Tcp, Instant, Instant)>,
    last: Option<EventId>,
    handed: Option<EventId>,
    /// How many times it subscribed (the first, and each after a drop).
    pub subscribed: u32,
}

impl Subscription {
    /// A subscription to the manager at `addr`, known as `host`, keeping its
    /// place under `state`. Nothing is dialled until [`Subscription::next`].
    pub fn new(addr: SocketAddr, token: Token, host: &str, state: &Path, version: &str) -> Subscription {
        Subscription {
            addr,
            token,
            host: host.to_string(),
            state: Some(state.to_path_buf()),
            role: Role::Daemon,
            version: version.to_string(),
            conn: None,
            last: read_last(state, host),
            handed: None,
            subscribed: 0,
        }
    }

    /// A watch for one CLI call (a spawn waiting on its seat's first order):
    /// subscribed now, from the journal's head as the manager's `hello` gives
    /// it, so it hears what happens from here on and nothing before. Its place
    /// is kept in memory only, so it never moves the daemon's
    /// `<state>/manager/<host>.last`; after a dropped connection it subscribes
    /// again from the last id it heard, as the daemon's does.
    pub fn at_head(addr: SocketAddr, token: Token, role: Role, version: &str) -> std::result::Result<Subscription, HostError> {
        let (conn, hello) = open(addr, &token, role, version, None)?;
        let mut sub = Subscription {
            addr,
            token,
            host: hello.host.0.clone(),
            state: None,
            role,
            version: version.to_string(),
            conn: None,
            last: hello.journal_head,
            handed: None,
            subscribed: 0,
        };
        sub.subscribe_on(conn)?;
        Ok(sub)
    }

    /// A watch for one CLI call that hears what came after event `from`,
    /// replayed from the journal where it already happened: a start's events
    /// after its own `seat.started` (wsp-037). Kept in memory, as
    /// [`Subscription::at_head`]'s is.
    pub fn after(addr: SocketAddr, token: Token, role: Role, version: &str, from: EventId) -> std::result::Result<Subscription, HostError> {
        let (conn, hello) = open(addr, &token, role, version, Some(from))?;
        let mut sub = Subscription {
            addr,
            token,
            host: hello.host.0.clone(),
            state: None,
            role,
            version: version.to_string(),
            conn: None,
            last: Some(from),
            handed: None,
            subscribed: 0,
        };
        sub.subscribe_on(conn)?;
        Ok(sub)
    }

    /// The one the local machine record names.
    pub fn from_record(state: &Path, version: &str) -> std::result::Result<Subscription, HostError> {
        let (local, token) = manager::read_record(state).ok_or_else(|| HostError::NoRecord(manager::record_path(state)))?;
        Ok(Subscription::new(local.socket(), token, &local.host, state, version))
    }

    /// The last id kept.
    pub fn last(&self) -> Option<EventId> {
        self.last
    }

    fn keep(&mut self) {
        let Some(id) = self.handed.take() else { return };
        if self.last.is_some_and(|l| l >= id) {
            return;
        }
        self.last = Some(id);
        let Some(state) = &self.state else { return };
        let path = last_path(state, &self.host);
        if let Some(d) = path.parent() {
            let _ = fs::create_dir_all(d);
        }
        let _ = crate::store::write_atomic(&path, &format!("{}\n", id.0));
    }

    fn subscribe(&mut self) -> std::result::Result<(), HostError> {
        let (conn, _) = open(self.addr, &self.token, self.role, &self.version, self.last)?;
        self.subscribe_on(conn)
    }

    fn subscribe_on(&mut self, mut conn: wsp_host::Tcp) -> std::result::Result<(), HostError> {
        let sub = Frame { id: None, req: None, msg: Message::Subscribe(Subscribe { from: self.last }) };
        conn.send(&sub).map_err(|e| HostError::Silent { addr: self.addr, why: format!("it did not take subscribe: {e:?}") })?;
        self.conn = Some((conn, Instant::now(), Instant::now()));
        self.subscribed += 1;
        Ok(())
    }

    /// The next frame within `wait`: an event (with its id), or the manager's
    /// beat. `Ok(None)` when nothing came, or when the connection dropped (the
    /// next call subscribes again from the last id kept). `Err` only when the
    /// manager could not be reached to subscribe.
    pub fn next(&mut self, wait: Duration) -> std::result::Result<Option<Frame>, HostError> {
        self.keep();
        if self.conn.is_none() {
            self.subscribe()?;
        }
        let patience = Duration::from_secs(BEAT_SECS) * MISSED_BEATS;
        let deadline = Instant::now() + wait;
        loop {
            let Some((conn, heard, sent)) = self.conn.as_mut() else { return Ok(None) };
            if heard.elapsed() >= patience {
                conn.shutdown();
                self.conn = None;
                return Ok(None);
            }
            if sent.elapsed() >= Duration::from_secs(BEAT_SECS) {
                let _ = conn.send(&beat_frame());
                *sent = Instant::now();
            }
            let left = deadline.saturating_duration_since(Instant::now());
            if left.is_zero() {
                return Ok(None);
            }
            match conn.recv(left.min(Duration::from_secs(BEAT_SECS))) {
                Ok(Some(f)) => {
                    *heard = Instant::now();
                    match f.id {
                        // Already heard: a replay that overlaps what was kept.
                        Some(id) if self.last.is_some_and(|l| id <= l) => continue,
                        Some(id) => self.handed = Some(id),
                        None if matches!(f.msg, Message::Beat(_)) => {}
                        None => continue,
                    }
                    return Ok(Some(f));
                }
                Ok(None) => {}
                Err(ConnError::Malformed(_)) => {}
                Err(_) => {
                    self.conn = None;
                    return Ok(None);
                }
            }
        }
    }

    /// Close the connection, keeping the last id handed out.
    pub fn close(&mut self) {
        self.keep();
        if let Some((conn, _, _)) = self.conn.take() {
            conn.shutdown();
        }
    }
}

impl Drop for Subscription {
    fn drop(&mut self) {
        if let Some((conn, _, _)) = self.conn.take() {
            conn.shutdown();
        }
    }
}
