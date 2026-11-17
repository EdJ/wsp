//! The seats this manager runs (`docs/agent-manager.md` 2.5, 3.2, 3.3; wsp-022):
//! `seat.spawn`, `seat.stop`, `order.deliver` and `order.cancel`, `seats` and
//! `seat.read`, over `wsp-host`'s in-process code.
//!
//! - **One compound-sup per seat, forked here.** The manager runs
//!   `compound-sup spawn --label <seat>` itself, with `COMPOUND_RUN_DIR` at
//!   `<exec_dir>/run`, so a seat's session sockets (and their `.label`
//!   sidecars) live under the manager's own directory. compound-sup is durable
//!   (`setsid`) and outlives the manager.
//! - **Keys.** `seat.spawn` is idempotent on `(row, attempt)`: a retry gets the
//!   same `seat.started`, and a spawn still forking gets that same answer when
//!   it comes, so one key never starts two agents. A key whose seat already
//!   ran and stopped, or a seat id already running under another key, is
//!   refused `duplicate`.
//! - **Epochs.** One per start of a seat, counted from the journal, so they run
//!   on across a restart. A `seat.stop` or `order.deliver` naming an older
//!   epoch than the seat's is refused `stale_epoch`.
//! - **Orders** go through `compound-sup deliver <socket> <id>`, whose id is its
//!   at-most-once key. Before it is asked the order is written to
//!   `<exec_dir>/orders/`; its answer is journalled and only then is the file
//!   removed. `delivered` is answered `order.delivered` and `order.acked` (and
//!   a `turn` that began): compound-sup says `delivered` only once the agent's
//!   own `UserPromptSubmit` hook has said the turn began. `failed <why>` is
//!   `order.refused` with compound-sup's `FailWhy`. A repeat of an answered
//!   order gets the same answer and is never typed again.
//! - **Restart.** The table is rebuilt from the `*.sock` files in
//!   `<exec_dir>/run`, each joined by its `.label` to the journal's last
//!   `seat.started` for that seat, and kept if `compound-sup vitals` answers
//!   for it. A seat the journal has running whose session is gone is reported
//!   `seat.stopped`. An order still in `<exec_dir>/orders/` is asked again under
//!   the same id, unless the journal already holds its answer.
//! - **`seats`** answers from the table, `compound-sup vitals` per seat and one
//!   `ps` for every seat's rss and shells; it renders no screen. `seat.read` is
//!   the one render, for a person's peek.
//!
//! Local seats are not gated (d1 Q12): a `gate: required` is refused
//! `unsupported` until the gate's lease is written here (section 5).

use std::collections::BTreeMap;
use std::fs;
use std::io::{BufRead, BufReader};
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::sync::atomic::Ordering;
use std::sync::mpsc::Sender;
use std::sync::{mpsc, Arc, MutexGuard};
use std::time::Duration;

use wsp_base::calls::Command;
use wsp_host::inproc::{self, Spawn, Stop};
use wsp_host::{Delivery, InProcess, Refusal as HostRefusal, Seat as HostSeat};
use wsp_host_api::{
    At, Epoch, Frame, Gate, Key, Message, OrderAcked, OrderCancel, OrderDeliver, OrderDelivered, OrderFail,
    OrderId, OrderRefused, Refusal, Refused, ReqId, SeatEpoch, SeatId, SeatRead, SeatRefused, SeatRow,
    SeatScreen, SeatSpawn, SeatStarted, SeatState, SeatStop, SeatStopped, SeatsList, Turn, TurnEdge, Workdir,
};

use crate::{now, Shared};

/// Under `<exec_dir>`: compound-sup's run directory, and the orders asked and
/// not yet answered.
pub const RUN_DIR: &str = "run";
pub const ORDERS_DIR: &str = "orders";

/// How long a spawn waits for compound-sup's handshake.
const HANDSHAKE: Duration = Duration::from_secs(5);
/// How long one `compound-sup deliver` is waited on before it is asked again
/// under the same id.
const DELIVER_WAIT: Duration = Duration::from_secs(60);
/// Between asks of a delivery that came back with no answer.
const ASK_AGAIN: Duration = Duration::from_secs(1);
/// What a seat's shells are called, for `seats`' count.
const SHELLS: &[&str] = &["sh", "bash", "zsh", "dash", "fish", "ksh", "tcsh", "csh"];

/// Who asked: the answer goes back to them with their `req`, once.
#[derive(Clone)]
pub(crate) struct Asker {
    /// The connection's client id; `None` for the manager's own asks (restart).
    pub(crate) client: Option<u64>,
    pub(crate) out: Option<Sender<Frame>>,
    pub(crate) req: Option<ReqId>,
}

impl Asker {
    pub(crate) fn nobody() -> Asker {
        Asker { client: None, out: None, req: None }
    }

    pub(crate) fn send(&self, msg: Message) {
        self.send_frame(Frame { id: None, req: self.req, msg });
    }

    pub(crate) fn send_frame(&self, frame: Frame) {
        if let Some(out) = &self.out {
            let _ = out.send(Frame { req: self.req, ..frame });
        }
    }

    fn refuse(&self, why: Refusal, detail: impl Into<String>) {
        self.send(Message::Refused(Refused { why, detail: detail.into() }));
    }
}

/// One start of a seat, running.
struct Running {
    key: Key,
    epoch: Epoch,
    /// compound-sup's pid: the session's whole life, and its own group.
    pid: u32,
    socket: PathBuf,
    render: Option<PathBuf>,
    started: SeatStarted,
    /// What it was spawned with, to tell a retry from another spawn on one key.
    spec: SeatSpawn,
    /// Asked to stop: the askers waiting on `seat.stopped`.
    stopping: Option<Vec<Asker>>,
    /// The supervisor's exit, when the manager is its parent and saw it.
    exit: Option<i32>,
    turn: Option<TurnEdge>,
    last_turn: Option<At>,
    last_word: Option<String>,
}

enum KeyState {
    /// Forking: every asker gets the answer when it comes.
    Spawning { seat: SeatId, waiters: Vec<Asker> },
    Running(SeatId),
    /// Ran and stopped: a retry cannot be answered `started`.
    Ended(SeatId, Epoch),
}

enum Order {
    /// Handed to compound-sup and not yet answered.
    InFlight { waiters: Vec<Asker> },
    /// Answered: the answer, sent again to whoever asks again.
    Answered(Vec<Message>),
}

#[derive(Default)]
pub(crate) struct Table {
    seats: BTreeMap<SeatId, Running>,
    keys: BTreeMap<Key, KeyState>,
    /// The latest epoch each seat id has had, running or not.
    epochs: BTreeMap<SeatId, Epoch>,
    orders: BTreeMap<OrderId, Order>,
}

impl Table {
    /// `hello`'s seats.
    pub(crate) fn epochs(&self) -> Vec<SeatEpoch> {
        self.seats.values().map(|s| SeatEpoch { seat: s.started.seat.clone(), epoch: s.epoch }).collect()
    }
}

fn table(shared: &Shared) -> MutexGuard<'_, Table> {
    shared.seats.lock().unwrap_or_else(|e| e.into_inner())
}

fn host(shared: &Shared) -> InProcess {
    match &shared.cfg.sup {
        Some(sup) => InProcess::with_sup(sup),
        None => InProcess::new(),
    }
}

fn sup_path(shared: &Shared) -> Option<PathBuf> {
    shared.cfg.sup.clone().or_else(inproc::compound_sup_binary)
}

/// Journal `msg` and send it to subscribers and to `to` (once, with its req);
/// a journal that cannot be written is logged and the asker still answered.
fn emit(shared: &Shared, msg: Message, to: &[Asker]) {
    let first = to.first();
    match shared.emit_for(msg.clone(), first) {
        Ok(_) => {}
        Err(e) => {
            shared.log.line(&format!("the journal could not be written, {} not journalled: {e}", kind(&msg)));
            if let Some(a) = first {
                a.send(msg.clone());
            }
        }
    }
    for a in to.iter().skip(1) {
        a.send(msg.clone());
    }
}

fn kind(msg: &Message) -> String {
    serde_json::to_value(msg).ok().and_then(|v| v.get("type").and_then(|t| t.as_str()).map(str::to_string)).unwrap_or_default()
}

// ---------------------------------------------------------------- seat.spawn

pub(crate) fn spawn(shared: &Arc<Shared>, asker: Asker, s: SeatSpawn) {
    let refuse = |why: Refusal, detail: String| {
        asker.send(Message::SeatRefused(SeatRefused { key: s.key.clone(), seat: s.seat.clone(), why, detail }))
    };
    let epoch = {
        let mut t = table(shared);
        match t.keys.get_mut(&s.key) {
            Some(KeyState::Running(seat)) => {
                let seat = seat.clone();
                let run = t.seats.get(&seat).expect("a running key's seat is in the table");
                return if same_spawn(&run.spec, &s) {
                    asker.send(Message::SeatStarted(run.started.clone()))
                } else {
                    refuse(Refusal::Duplicate, format!("key {}/{} already runs seat {} with other arguments", s.key.row.0, s.key.attempt, seat.0))
                };
            }
            Some(KeyState::Spawning { seat, waiters }) => {
                return if *seat == s.seat {
                    waiters.push(asker.clone())
                } else {
                    refuse(Refusal::Duplicate, format!("key {}/{} is already starting seat {}", s.key.row.0, s.key.attempt, seat.0))
                };
            }
            Some(KeyState::Ended(seat, epoch)) => {
                return refuse(
                    Refusal::Duplicate,
                    format!("key {}/{} already ran seat {} (epoch {}), which has stopped", s.key.row.0, s.key.attempt, seat.0, epoch.0),
                );
            }
            None => {}
        }
        if let Some(run) = t.seats.get(&s.seat) {
            return refuse(Refusal::Duplicate, format!("seat {} already runs under key {}/{}", s.seat.0, run.key.row.0, run.key.attempt));
        }
        if t.keys.values().any(|k| matches!(k, KeyState::Spawning { seat, .. } if *seat == s.seat)) {
            return refuse(Refusal::Duplicate, format!("seat {} is already starting under another key", s.seat.0));
        }
        if let Err((why, detail)) = admissible(shared, &t, &s) {
            return refuse(why, detail);
        }
        t.keys.insert(s.key.clone(), KeyState::Spawning { seat: s.seat.clone(), waiters: vec![asker] });
        Epoch(t.epochs.get(&s.seat).map_or(1, |e| e.0 + 1))
    };

    let shared = shared.clone();
    std::thread::spawn(move || {
        let forked = fork(&shared, &s);
        let mut t = table(&shared);
        let waiters = match t.keys.remove(&s.key) {
            Some(KeyState::Spawning { waiters, .. }) => waiters,
            _ => vec![],
        };
        match forked {
            Ok(f) => {
                let started = SeatStarted {
                    key: s.key.clone(),
                    seat: s.seat.clone(),
                    epoch,
                    pid: f.pid,
                    sockets: std::iter::once(&f.socket).chain(f.render.as_ref()).map(|p| p.display().to_string()).collect(),
                };
                t.epochs.insert(s.seat.clone(), epoch);
                t.keys.insert(s.key.clone(), KeyState::Running(s.seat.clone()));
                t.seats.insert(
                    s.seat.clone(),
                    Running {
                        key: s.key.clone(),
                        epoch,
                        pid: f.pid,
                        socket: f.socket,
                        render: f.render,
                        started: started.clone(),
                        spec: SeatSpawn { first_order: None, ..s.clone() },
                        stopping: None,
                        exit: None,
                        turn: None,
                        last_turn: None,
                        last_word: None,
                    },
                );
                shared.log.line(&format!("seat {} started (epoch {}, pid {})", s.seat.0, epoch.0, f.pid));
                emit(&shared, Message::SeatStarted(started), &waiters);
                drop(t);
                if let Some(child) = f.child {
                    reap(&shared, child, f.pid, s.seat.clone(), epoch);
                }
                if let Some(first) = &s.first_order {
                    let o = OrderDeliver { order: first.order.clone(), seat: s.seat.clone(), epoch, text: first.text.clone(), now: false };
                    deliver(&shared, Asker::nobody(), o);
                }
            }
            Err(detail) => {
                drop(t);
                shared.log.line(&format!("seat {} did not start: {detail}", s.seat.0));
                let refused = SeatRefused { key: s.key.clone(), seat: s.seat.clone(), why: Refusal::Failed, detail };
                for w in &waiters {
                    w.send(Message::SeatRefused(refused.clone()));
                }
            }
        }
    });
}

/// A retry is the same spawn: the same seat, agent and place. An adopted
/// seat's arguments were not kept, so for it the same seat is enough.
fn same_spawn(running: &SeatSpawn, b: &SeatSpawn) -> bool {
    running.seat == b.seat
        && (is_adopted(running) || (running.agent == b.agent && running.workdir == b.workdir && running.gate == b.gate))
}

/// What the manager itself refuses before forking: what is beyond it, never a
/// judgement of wsp's.
fn admissible(shared: &Shared, t: &Table, s: &SeatSpawn) -> Result<(), (Refusal, String)> {
    if s.agent.argv.is_empty() {
        return Err((Refusal::Malformed, "the agent's argv is empty".into()));
    }
    if s.gate == Gate::Required {
        return Err((Refusal::Unsupported, "local seats are not gated (d1 Q12); the gate's lease is written from section 5's step".into()));
    }
    match &s.workdir {
        Workdir::Cwd(_) => {}
        Workdir::Tree(tree) => {
            return Err((
                Refusal::NoTree,
                format!("trees are not placed on hosts until Q6 (tree {} on {}); send the seat's cwd", tree.row.0, tree.host.0),
            ))
        }
    }
    let starting = t.keys.values().filter(|k| matches!(k, KeyState::Spawning { .. })).count();
    if t.seats.len() + starting >= shared.cfg.seats as usize {
        return Err((Refusal::Full, format!("{} seats run or start here, its capacity", shared.cfg.seats)));
    }
    Ok(())
}

struct Forked {
    pid: u32,
    socket: PathBuf,
    render: Option<PathBuf>,
    /// The process the manager ran, when it is the session itself (the real
    /// compound-sup never forks again), so its exit is the seat's end.
    child: Option<wsp_base::calls::Child>,
}

/// `compound-sup spawn --label <seat> … -- <argv>`, run here, its handshake read.
fn fork(shared: &Shared, s: &SeatSpawn) -> Result<Forked, String> {
    let sup = sup_path(shared).ok_or("compound-sup not found: set $COMPOUND_SUP or put it on PATH")?;
    let Workdir::Cwd(cwd) = &s.workdir else { unreachable!("refused before forking") };
    let (program, args) = s.agent.argv.split_first().expect("refused before forking");
    let mut env: BTreeMap<String, String> =
        s.agent.env.iter().filter_map(|name| std::env::var(name).ok().map(|v| (name.clone(), v))).collect();
    env.insert(wsp_base::base::seat::SEAT_ENV.into(), s.seat.0.clone());
    let spawn = Spawn { seat: &s.seat.0, kind: Some(s.agent.kind.clone()), cwd: cwd.clone(), env, program, args };
    let mut cmd = inproc::sup_command(&sup, &spawn);
    // After `sup_command`, so this run directory is the one it uses.
    cmd.env("COMPOUND_RUN_DIR", shared.run_dir());
    let mut child = cmd.spawn().map_err(|e| format!("{} did not start: {e}", sup.display()))?;
    let Some(stdout) = child.stdout.take() else {
        let _ = child.kill();
        return Err("compound-sup gave no handshake pipe".into());
    };
    match handshake(stdout, HANDSHAKE) {
        Some(Handshake { sup: pid, socket, render }) => {
            let pid = pid.unwrap_or(child.id());
            let child = (pid == child.id()).then_some(child);
            Ok(Forked { pid, socket, render, child })
        }
        None => {
            host(shared).signal_group(child.id(), "KILL");
            let _ = child.kill();
            Err("compound-sup never announced a socket".into())
        }
    }
}

struct Handshake {
    sup: Option<u32>,
    socket: PathBuf,
    render: Option<PathBuf>,
}

/// compound-sup's handshake (`sup <pid>`, `pid <pid>`, `socket <path>`,
/// `render <path>`), read with a deadline. `inproc::read_handshake` less the
/// `sup` line, which the manager needs: it is the session's pid.
fn handshake(stdout: std::process::ChildStdout, timeout: Duration) -> Option<Handshake> {
    let (tx, rx) = mpsc::channel();
    std::thread::spawn(move || {
        let mut reader = BufReader::new(stdout);
        let (mut sup, mut socket, mut render) = (None, None, None);
        for _ in 0..4 {
            let mut line = String::new();
            if reader.read_line(&mut line).unwrap_or(0) == 0 {
                break;
            }
            let line = line.trim();
            if let Some(pid) = line.strip_prefix("sup ") {
                sup = pid.trim().parse().ok();
            } else if let Some(path) = line.strip_prefix("socket ") {
                socket = Some(PathBuf::from(path));
            } else if let Some(path) = line.strip_prefix("render ") {
                render = Some(PathBuf::from(path));
                break;
            }
        }
        let _ = tx.send(socket.map(|socket| Handshake { sup, socket, render }));
    });
    rx.recv_timeout(timeout).ok().flatten()
}

/// Wait on the supervisor the manager is parent of: its exit ends the seat.
fn reap(shared: &Arc<Shared>, mut child: wsp_base::calls::Child, pid: u32, seat: SeatId, epoch: Epoch) {
    let shared = shared.clone();
    std::thread::spawn(move || {
        let exit = child.wait().ok().and_then(|s| s.code());
        let mut t = table(&shared);
        match t.seats.get_mut(&seat) {
            Some(run) if run.epoch == epoch && run.pid == pid => {
                run.exit = exit;
                // A stop in progress reports it; otherwise the seat ended itself.
                if run.stopping.is_none() {
                    drop(t);
                    ended(&shared, &seat, epoch, vec![pid], vec![]);
                }
            }
            _ => {}
        }
    });
}

/// A seat's start ended: out of the table, `seat.stopped` journalled, and
/// whoever waited on it answered. Once per start, whoever gets here first.
fn ended(shared: &Shared, seat: &SeatId, epoch: Epoch, gone: Vec<u32>, mut to: Vec<Asker>) {
    if shared.stopping.load(Ordering::SeqCst) {
        return;
    }
    let mut t = table(shared);
    if !t.seats.get(seat).is_some_and(|r| r.epoch == epoch) {
        return;
    }
    let Running { key, exit, stopping, render, .. } = t.seats.remove(seat).expect("found just above");
    t.keys.insert(key, KeyState::Ended(seat.clone(), epoch));
    if let Some(waiters) = stopping {
        to.extend(waiters);
    }
    let renderer_left = render.is_some_and(|r| std::os::unix::net::UnixStream::connect(&r).is_ok());
    shared.log.line(&format!("seat {} stopped (epoch {}, exit {exit:?}, ended {gone:?})", seat.0, epoch.0));
    emit(shared, Message::SeatStopped(SeatStopped { seat: seat.clone(), epoch, exit, ended: gone, renderer_left }), &to);
}

// ---------------------------------------------------------------- seat.stop

/// Check `epoch` against the seat's; the refusal's reason when it is not its.
fn fence(run: &Running, epoch: Epoch) -> Result<(), (Refusal, String)> {
    if epoch < run.epoch {
        Err((Refusal::StaleEpoch, format!("epoch {} is older than seat {}'s epoch {}", epoch.0, run.started.seat.0, run.epoch.0)))
    } else if epoch > run.epoch {
        Err((Refusal::NoSeat, format!("seat {} has had no epoch {} (it is at {})", run.started.seat.0, epoch.0, run.epoch.0)))
    } else {
        Ok(())
    }
}

pub(crate) fn stop(shared: &Arc<Shared>, asker: Asker, s: SeatStop) {
    let (pid, socket) = {
        let mut t = table(shared);
        let Some(run) = t.seats.get_mut(&s.seat) else {
            let detail = match t.epochs.get(&s.seat) {
                Some(e) => format!("seat {} is not running (its last epoch, {}, has stopped)", s.seat.0, e.0),
                None => format!("this manager runs no seat {}", s.seat.0),
            };
            let why = if t.epochs.get(&s.seat).is_some_and(|e| s.epoch < *e) { Refusal::StaleEpoch } else { Refusal::NoSeat };
            return asker.refuse(why, detail);
        };
        if let Err((why, detail)) = fence(run, s.epoch) {
            return asker.refuse(why, detail);
        }
        if let Some(waiters) = &mut run.stopping {
            waiters.push(asker);
            return;
        }
        run.stopping = Some(vec![asker]);
        (run.pid, run.socket.clone())
    };
    let shared = shared.clone();
    std::thread::spawn(move || {
        let host = host(&shared);
        let tree = host.tree_of(pid);
        host.stop(Stop {
            group: pid,
            watch: Some(pid),
            grace: Duration::from_secs(s.grace_secs.into()),
            poll: Duration::from_millis(50),
            clock: &wsp_base::util::Wall,
        });
        // The rest of the tree a moment to follow its group's leader.
        let mut left = host.alive(&tree);
        for _ in 0..20 {
            if left.is_empty() {
                break;
            }
            std::thread::sleep(Duration::from_millis(25));
            left = host.alive(&tree);
        }
        let gone: Vec<u32> = tree.into_iter().filter(|p| !left.contains(p)).collect();
        host.clear_socket(&socket, &["label"]);
        ended(&shared, &s.seat, s.epoch, gone, vec![]);
    });
}

// ---------------------------------------------------------------- orders

/// The order asked of compound-sup and not yet answered, kept on disk so a
/// restart asks again under the same id.
fn pending_path(shared: &Shared, order: &OrderId) -> PathBuf {
    let hex: String = order.0.bytes().map(|b| format!("{b:02x}")).collect();
    shared.cfg.exec_dir.join(ORDERS_DIR).join(format!("{hex}.json"))
}

fn write_pending(shared: &Shared, o: &OrderDeliver) -> std::io::Result<()> {
    let path = pending_path(shared, &o.order);
    fs::create_dir_all(path.parent().expect("under the orders dir"))?;
    let tmp = path.with_extension("tmp");
    fs::write(&tmp, serde_json::to_vec(o).map_err(std::io::Error::other)?)?;
    fs::rename(tmp, path)
}

fn refuse_order(shared: &Shared, t: &mut Table, asker: Asker, o: &OrderDeliver, why: OrderFail) {
    let msg = Message::OrderRefused(OrderRefused { order: o.order.clone(), seat: o.seat.clone(), why });
    emit(shared, msg.clone(), &[asker]);
    t.orders.insert(o.order.clone(), Order::Answered(vec![msg]));
}

pub(crate) fn deliver(shared: &Arc<Shared>, asker: Asker, o: OrderDeliver) {
    {
        let mut t = table(shared);
        match t.orders.get_mut(&o.order) {
            Some(Order::Answered(said)) => {
                for msg in said.iter() {
                    asker.send(msg.clone());
                }
                return;
            }
            Some(Order::InFlight { waiters }) => return waiters.push(asker),
            None => {}
        }
        let Some(run) = t.seats.get(&o.seat) else {
            let why = if t.epochs.get(&o.seat).is_some_and(|e| o.epoch < *e) { OrderFail::StaleEpoch } else { OrderFail::NoSeat };
            return refuse_order(shared, &mut t, asker, &o, why);
        };
        match fence(run, o.epoch) {
            Err((Refusal::StaleEpoch, _)) => return refuse_order(shared, &mut t, asker, &o, OrderFail::StaleEpoch),
            Err(_) => return refuse_order(shared, &mut t, asker, &o, OrderFail::NoSeat),
            Ok(()) => {}
        }
        if run.stopping.is_some() {
            return refuse_order(shared, &mut t, asker, &o, OrderFail::SessionGone);
        }
        if let Err(e) = write_pending(shared, &o) {
            // Not asked without the record a restart would ask again from.
            shared.log.line(&format!("order {}: not asked, its record could not be written: {e}", o.order.0));
            return asker.refuse(Refusal::Failed, format!("the order's record could not be written: {e}"));
        }
        t.orders.insert(o.order.clone(), Order::InFlight { waiters: vec![asker] });
    }
    let shared = shared.clone();
    std::thread::spawn(move || ask(&shared, o));
}

/// Ask compound-sup until it answers, then answer. The id is its at-most-once
/// key, so asking again (a wait that ran out, a restart) is the same order.
fn ask(shared: &Arc<Shared>, o: OrderDeliver) {
    let Some(sup) = sup_path(shared) else {
        shared.log.line(&format!("order {}: compound-sup not found; left for the next start", o.order.0));
        return;
    };
    let seat = HostSeat::new(&o.seat.0);
    let mut tries = 0u32;
    let outcome = loop {
        if shared.stopping.load(Ordering::SeqCst) {
            return;
        }
        let socket = {
            let t = table(shared);
            t.seats.get(&o.seat).filter(|r| r.epoch == o.epoch).map(|r| r.socket.clone())
        };
        let Some(socket) = socket else { break Err(OrderFail::SessionGone) };
        match inproc::deliver_with(&sup, &socket, &o.order.0, &o.text, DELIVER_WAIT, &seat) {
            // `deliver_with` answers only `Acked` for compound-sup's `delivered`.
            Ok(Delivery::Acked | Delivery::Started | Delivery::Unconfirmed) => break Ok(now()),
            Err(HostRefusal::Failed(why)) => break Err(fail_of(&why)),
            Err(HostRefusal::NoSeat(_)) => break Err(OrderFail::SessionGone),
            Err(e) => {
                tries += 1;
                if tries == 1 || tries.is_multiple_of(30) {
                    shared.log.line(&format!("order {}: no answer yet ({e}); asked again under the same id", o.order.0));
                }
                std::thread::sleep(ASK_AGAIN);
            }
        }
    };
    answered(shared, &o, outcome);
}

/// compound-sup's `failed <id> <why>`, its why read as an [`OrderFail`]; the
/// host's message is `<seat>: <why>`.
fn fail_of(said: &str) -> OrderFail {
    let why = said.rsplit_once(": ").map_or(said, |(_, w)| w).trim();
    let word = why.split_whitespace().next().unwrap_or("").replace('-', "_");
    serde_json::from_value(serde_json::Value::String(word)).unwrap_or(OrderFail::Unknown)
}

fn answered(shared: &Shared, o: &OrderDeliver, outcome: Result<At, OrderFail>) {
    let mut t = table(shared);
    if shared.stopping.load(Ordering::SeqCst) {
        // The next start asks again from the record and answers then.
        return;
    }
    let waiters = match t.orders.remove(&o.order) {
        Some(Order::InFlight { waiters }) => waiters,
        _ => vec![],
    };
    let said = match outcome {
        Ok(began) => {
            let delivered = Message::OrderDelivered(OrderDelivered { order: o.order.clone(), seat: o.seat.clone() });
            let acked = Message::OrderAcked(OrderAcked { order: o.order.clone(), seat: o.seat.clone(), turn_began: began });
            emit(shared, delivered.clone(), &waiters);
            emit(shared, acked.clone(), &waiters);
            if let Some(run) = t.seats.get_mut(&o.seat).filter(|r| r.epoch == o.epoch) {
                run.turn = Some(TurnEdge::Began);
                run.last_turn = Some(began);
            }
            let turn = Turn { seat: o.seat.clone(), epoch: o.epoch, edge: TurnEdge::Began, at: began, order: Some(o.order.clone()) };
            emit(shared, Message::Turn(turn), &[]);
            vec![delivered, acked]
        }
        Err(why) => {
            let refused = Message::OrderRefused(OrderRefused { order: o.order.clone(), seat: o.seat.clone(), why });
            emit(shared, refused.clone(), &waiters);
            vec![refused]
        }
    };
    shared.log.line(&format!("order {} to {}: {}", o.order.0, o.seat.0, said.iter().map(kind).collect::<Vec<_>>().join(", ")));
    t.orders.insert(o.order.clone(), Order::Answered(said));
    let _ = fs::remove_file(pending_path(shared, &o.order));
}

pub(crate) fn cancel(shared: &Shared, asker: Asker, c: OrderCancel) {
    let mut t = table(shared);
    match t.orders.get(&c.order) {
        Some(Order::Answered(said)) => {
            for msg in said {
                asker.send(msg.clone());
            }
        }
        Some(Order::InFlight { .. }) => asker.refuse(
            Refusal::Failed,
            format!("order {} is already with compound-sup, which owns its delivery; its answer follows", c.order.0),
        ),
        None => {
            let msg = Message::OrderRefused(OrderRefused { order: c.order.clone(), seat: c.seat.clone(), why: OrderFail::Cancelled });
            emit(shared, msg.clone(), &[asker]);
            t.orders.insert(c.order, Order::Answered(vec![msg]));
        }
    }
}

// ---------------------------------------------------------------- seats and seat.read

/// `compound-sup vitals <socket>`: its lines when the session answers.
fn vitals(sup: &Path, socket: &Path) -> Option<String> {
    let out = Command::new(sup).arg("vitals").arg(socket).stdin(Stdio::null()).stderr(Stdio::null()).output().ok()?;
    out.status.success().then(|| String::from_utf8_lossy(&out.stdout).into_owned())
}

/// `ps -A -o pid=,ppid=,rss=,comm=`, read.
fn rows(ps: &str) -> Vec<(u32, u32, u64, String)> {
    ps.lines()
        .filter_map(|l| {
            let mut w = l.split_whitespace();
            let pid = w.next()?.parse().ok()?;
            let ppid = w.next()?.parse().ok()?;
            let rss = w.next()?.parse().ok()?;
            let comm = w.collect::<Vec<_>>().join(" ");
            Some((pid, ppid, rss, comm))
        })
        .collect()
}

/// A seat's rss in bytes and its shells, out of the one `ps`.
fn measure(rows: &[(u32, u32, u64, String)], root: u32) -> (u64, u32) {
    let mut tree = vec![root];
    let mut i = 0;
    while i < tree.len() {
        let parent = tree[i];
        tree.extend(rows.iter().filter(|r| r.1 == parent && !tree.contains(&r.0)).map(|r| r.0).collect::<Vec<_>>());
        i += 1;
    }
    let mine: Vec<&(u32, u32, u64, String)> = rows.iter().filter(|r| tree.contains(&r.0)).collect();
    let rss = mine.iter().map(|r| r.2 * 1024).sum();
    let shells = mine
        .iter()
        .filter(|r| r.0 != root)
        .filter(|r| {
            let base = r.3.rsplit('/').next().unwrap_or("").trim_start_matches('-');
            SHELLS.contains(&base)
        })
        .count() as u32;
    (rss, shells)
}

pub(crate) fn list(shared: &Shared, asker: Asker) {
    struct Look {
        seat: SeatId,
        epoch: Epoch,
        pid: u32,
        socket: PathBuf,
        stopping: bool,
        turn: Option<TurnEdge>,
        last_turn: Option<At>,
        last_word: Option<String>,
    }
    let looks: Vec<Look> = table(shared)
        .seats
        .values()
        .map(|r| Look {
            seat: r.started.seat.clone(),
            epoch: r.epoch,
            pid: r.pid,
            socket: r.socket.clone(),
            stopping: r.stopping.is_some(),
            turn: r.turn,
            last_turn: r.last_turn,
            last_word: r.last_word.clone(),
        })
        .collect();
    let ps = if looks.is_empty() { Vec::new() } else { host(shared).table("pid=,ppid=,rss=,comm=", false).map(|t| rows(&t)).unwrap_or_default() };
    let sup = sup_path(shared);
    let mut seats = Vec::new();
    for l in looks {
        let answers = sup.as_deref().and_then(|s| vitals(s, &l.socket)).is_some();
        let running = ps.iter().any(|r| r.0 == l.pid);
        if !answers && !running && !l.stopping && !ps.is_empty() {
            // Its session is gone and nothing said so: it ended.
            ended(shared, &l.seat, l.epoch, vec![], vec![]);
            continue;
        }
        let state = if l.stopping {
            SeatState::Stopping
        } else if l.turn == Some(TurnEdge::Began) {
            SeatState::Working
        } else if !answers {
            SeatState::Starting
        } else {
            SeatState::Idle
        };
        let (rss_bytes, shells) = measure(&ps, l.pid);
        seats.push(SeatRow { seat: l.seat, epoch: l.epoch, state, rss_bytes, shells, last_turn: l.last_turn, last_word: l.last_word });
    }
    asker.send(Message::SeatsList(SeatsList { seats }));
}

pub(crate) fn read(shared: &Shared, asker: Asker, r: SeatRead) {
    let found = table(shared).seats.get(&r.seat).map(|s| (s.epoch, s.render.clone()));
    let Some((epoch, render)) = found else { return asker.refuse(Refusal::NoSeat, format!("this manager runs no seat {}", r.seat.0)) };
    let Some(render) = render else { return asker.refuse(Refusal::Failed, format!("seat {} has no render socket", r.seat.0)) };
    match host(shared).read_screen(&render) {
        Ok(text) => asker.send(Message::SeatScreen(SeatScreen { seat: r.seat, epoch, text })),
        Err(e) => asker.refuse(Refusal::Failed, format!("seat {}'s screen could not be read: {e}", r.seat.0)),
    }
}

// ---------------------------------------------------------------- restart

/// Rebuild the table at start: the journal's seats and answers, joined by label
/// to the sessions in `<exec_dir>/run`; then ask again what was asked and not
/// answered. Runs before any client is served.
pub(crate) fn adopt(shared: &Arc<Shared>) {
    let frames = {
        let journal = shared.journal.lock().unwrap_or_else(|e| e.into_inner());
        match journal.after(None) {
            Ok((frames, _)) => frames,
            Err(e) => {
                shared.log.line(&format!("the journal could not be read to adopt seats: {e}"));
                return;
            }
        }
    };
    let mut live: BTreeMap<SeatId, SeatStarted> = BTreeMap::new();
    let mut t = table(shared);
    for f in frames {
        match f.msg {
            Message::SeatStarted(s) => {
                let e = t.epochs.entry(s.seat.clone()).or_insert(s.epoch);
                *e = (*e).max(s.epoch);
                t.keys.insert(s.key.clone(), KeyState::Running(s.seat.clone()));
                live.insert(s.seat.clone(), s);
            }
            Message::SeatStopped(s) => {
                if let Some(started) = live.get(&s.seat).filter(|l| l.epoch == s.epoch) {
                    t.keys.insert(started.key.clone(), KeyState::Ended(s.seat.clone(), s.epoch));
                    live.remove(&s.seat);
                }
            }
            Message::OrderDelivered(OrderDelivered { ref order, .. })
            | Message::OrderAcked(OrderAcked { ref order, .. })
            | Message::OrderRefused(OrderRefused { ref order, .. }) => {
                let order = order.clone();
                match t.orders.entry(order).or_insert_with(|| Order::Answered(vec![])) {
                    Order::Answered(said) => said.push(f.msg),
                    Order::InFlight { .. } => {}
                }
            }
            _ => {}
        }
    }

    // The sessions, by their labels.
    let sup = sup_path(shared);
    let mut sessions: BTreeMap<SeatId, PathBuf> = BTreeMap::new();
    for entry in fs::read_dir(shared.run_dir()).into_iter().flatten().flatten() {
        let path = entry.path();
        if path.extension().and_then(|e| e.to_str()) != Some("sock") {
            continue;
        }
        let Ok(label) = fs::read_to_string(path.with_extension("label")) else { continue };
        let seat = SeatId(label.trim().to_string());
        if !live.contains_key(&seat) {
            shared.log.line(&format!("{}: labelled {}, which the journal has not running here; left alone", path.display(), seat.0));
            continue;
        }
        if sup.as_deref().and_then(|s| vitals(s, &path)).is_some() {
            sessions.insert(seat, path);
        }
    }
    let mut gone = Vec::new();
    for (seat, started) in live {
        match sessions.remove(&seat) {
            Some(socket) => {
                let render = started.sockets.get(1).map(PathBuf::from);
                shared.log.line(&format!("seat {} adopted (epoch {}, pid {}, {})", seat.0, started.epoch.0, started.pid, socket.display()));
                t.seats.insert(
                    seat.clone(),
                    Running {
                        key: started.key.clone(),
                        epoch: started.epoch,
                        pid: started.pid,
                        socket,
                        render,
                        spec: adopted_spec(&started),
                        started,
                        stopping: None,
                        exit: None,
                        turn: None,
                        last_turn: None,
                        last_word: None,
                    },
                );
            }
            None => gone.push(started),
        }
    }
    // Asked and not answered before the restart.
    let mut again = Vec::new();
    for entry in fs::read_dir(shared.cfg.exec_dir.join(ORDERS_DIR)).into_iter().flatten().flatten() {
        let path = entry.path();
        if path.extension().and_then(|e| e.to_str()) != Some("json") {
            continue;
        }
        let Some(o) = fs::read(&path).ok().and_then(|b| serde_json::from_slice::<OrderDeliver>(&b).ok()) else {
            shared.log.line(&format!("{}: an order record this build cannot read; left", path.display()));
            continue;
        };
        if matches!(t.orders.get(&o.order), Some(Order::Answered(said)) if !said.is_empty()) {
            // Answered and journalled; only the record's removal was lost.
            let _ = fs::remove_file(&path);
            continue;
        }
        t.orders.insert(o.order.clone(), Order::InFlight { waiters: vec![] });
        again.push(o);
    }
    drop(t);

    for started in gone {
        // Put back so `ended` finds it: it ended while the manager was down.
        let mut t = table(shared);
        t.seats.insert(
            started.seat.clone(),
            Running {
                key: started.key.clone(),
                epoch: started.epoch,
                pid: started.pid,
                socket: started.sockets.first().map(PathBuf::from).unwrap_or_default(),
                render: None,
                spec: adopted_spec(&started),
                started: started.clone(),
                stopping: None,
                exit: None,
                turn: None,
                last_turn: None,
                last_word: None,
            },
        );
        drop(t);
        shared.log.line(&format!("seat {} (epoch {}) ended while the manager was down", started.seat.0, started.epoch.0));
        ended(shared, &started.seat, started.epoch, vec![], vec![]);
    }
    for o in again {
        shared.log.line(&format!("order {} to {}: asked again under the same id", o.order.0, o.seat.0));
        let shared = shared.clone();
        std::thread::spawn(move || ask(&shared, o));
    }
}

/// An adopted seat's spawn arguments are not in the journal; a retry of its
/// key is answered by key and seat alone.
fn adopted_spec(started: &SeatStarted) -> SeatSpawn {
    SeatSpawn {
        key: started.key.clone(),
        seat: started.seat.clone(),
        agent: wsp_host_api::AgentSpec { argv: vec![], env: vec![], kind: String::new(), model: None },
        workdir: Workdir::Cwd(String::new()),
        gate: Gate::None,
        first_order: None,
    }
}

/// Whether this is an adopted seat's spec, whose arguments were not kept.
fn is_adopted(spec: &SeatSpawn) -> bool {
    spec.agent.argv.is_empty()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_fail_why_is_read_from_the_hosts_message() {
        assert_eq!(fail_of("cpd-1: busy"), OrderFail::Busy);
        assert_eq!(fail_of("cpd-1: no_composer seen in 30s"), OrderFail::NoComposer);
        assert_eq!(fail_of("cpd-1: session-gone"), OrderFail::SessionGone);
        assert_eq!(fail_of("cpd-1: teleported"), OrderFail::Unknown);
    }

    #[test]
    fn rss_and_shells_come_from_one_table() {
        let ps = "  10   1  1000 /usr/local/bin/compound-sup\n  11  10  2000 claude\n  12  11   100 /bin/zsh\n  13  12    50 -zsh\n  14   1  9999 other\n";
        let rows = rows(ps);
        assert_eq!(measure(&rows, 10), ((1000 + 2000 + 100 + 50) * 1024, 2));
        assert_eq!(measure(&rows, 99), (0, 0));
    }
}
