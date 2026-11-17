//! The host seam: every OS operation a seat needs, behind one trait
//! (`docs/agent-manager.md` section 6.2, steps M0 and M1).
//!
//! [`Host`] is what [`crate::place_compound`] and [`crate::place_super`] reach
//! the operating system through: spawning a seat's processes, stopping them,
//! its pids and process tree, the screen and state its renderer reports,
//! delivering an order, and the hook files and spend tallies its agent's hooks
//! write, and whether its hook log shows a turn begun. The root crate reaches
//! it for a seat's transcripts: where a session was written, its bytes, and
//! opencode's export and database. Those are the OS halves of the two backends
//! (section 2.2). The *decisions* stay with the backends: which seat, what to
//! run in it, when a hook's word has aged out, whether a seat will take a
//! prompt, what a seat record says. What a transcript means (what ran, what it
//! cost, what the turn did) stays with wsp's parsers, which take what is read
//! here.
//!
//! The methods are grouped by the host-api message they become at M1
//! (section 3.2): `seat.spawn`, `seat.stop`, `seats`, `seat.read`,
//! `order.deliver`, `seat.said`/`seat.spent`, `seat.did`, `turn`, `beat`
//! (the host's facts: disk, load, memory, swap, thermal state) and `hello`.
//! Each takes plain values (paths, pids, ids, text), never a seat record, so a
//! call turns into a message without a wsp type crossing the wire.
//!
//! [`InProcess`] is the first implementation, and it is not here: it is
//! `wsp-host` (`crates/host`, wsp-020), which depends on `wsp-base` and
//! `wsp-host-api` and on nothing of the store or this crate, so the agent
//! manager can link it. This module is the thin adapter over it: the trait,
//! [`LOCAL`], and an `impl Host for InProcess` whose every method calls the
//! in-process host's method of the same name. Everything else `wsp-host`'s
//! in-process module holds (the spawn and stop types, the hook file names,
//! the transcript paths) is re-exported here whole, so a caller names it as
//! `host::…` as it did before the move. At M1 a second implementation,
//! [`Tcp`] (`host_tcp.rs`, wsp-025), sends the same calls as messages to the
//! host's manager, chosen by `WSP_HOST=inproc|manager` ([`chosen`]; `inproc`
//! until wsp-029).
//!
//! The `ps`, `kill` and `lsof` of seat and tree work are there too, as
//! `seats`'s process table, signals and working directories (wsp-008). Tree
//! work's check_tree sweep, the throttle's and gc's liveness probes and a
//! check's kill on timeout ask [`LOCAL`], so at M3 their jobs move and their
//! calls do not.
//!
//! The layers below this crate (base, store) cannot name the host, so where
//! they need a fact (a pid's liveness, when it started) their callers pass the
//! host's answer in: `sharing::Alive`, `checks::StartOf`.
//!
//! Not in the host yet: tree work's jobs (section 1.3, which stays wsp's), the
//! far machine's ssh (`far.rs`'s [`crate::far::Wire`], M2's subject), and the
//! doctor's checks of installed hook snippets, which read config rather than a
//! seat.

use std::collections::BTreeSet;
use std::io::BufRead;
use std::path::{Path, PathBuf};
use std::time::Duration;

use serde_json::Value;

use crate::place::{Delivery, Result, Seat, State};

/// What goes with the in-process host (the spawn and stop types, the hook file
/// names, the transcript paths), named here as before the move. Its
/// `InProcess` is wrapped below, and the local type shadows the glob.
pub use wsp_host::inproc::*;

/// M1's second implementation, over the manager's connection (wsp-025), and
/// `WSP_HOST`'s choice between the two.
pub use crate::host_tcp::{choose, chosen, Choice, HostError, Subscription, Tcp};

/// What a stop ended, as `seat.stopped` says it (`docs/agent-manager.md` 3.2).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Ended {
    /// The pids that ended.
    pub pids: Vec<u32>,
    /// The agent's exit status, when it exited rather than was signalled.
    pub exit: Option<i32>,
}

/// Every OS operation a seat needs, grouped by the host-api message it
/// becomes (`docs/agent-manager.md` section 3.2).
pub trait Host: Sync {
    // ---- seat.spawn / seat.started
    /// `compound-sup spawn`, its handshake read.
    fn spawn_session(&self, spawn: &Spawn<'_>) -> Result<Session>;
    /// A headless agent forked with its feed, in a group of its own.
    fn spawn_headless(&self, spawn: &Headless<'_>) -> Result<Forked>;
    /// Whether this host runs [`Host::spawn_ordered`]: it has events to wait
    /// by. The in-process host has none, and its callers start, poll and tell.
    fn hears_orders(&self) -> bool {
        false
    }
    /// `seat.spawn` carrying its first order, then `seat.started` and that
    /// order's `order.acked` / `order.refused` waited for as the host's events
    /// (`docs/agent-manager.md` 4.3): the seat within `wait`, the order's answer
    /// within what is left of it. Once `seat.started` is heard the session is
    /// the answer: an order's answer that could not be heard (the events
    /// dropped) is [`crate::place::Heard::Unanswered`], never an error that
    /// would lose a running seat. The in-process host has no events.
    fn spawn_ordered(&self, _spawn: &Spawn<'_>, _order: &crate::place::FirstOrder, _wait: Duration) -> Result<(Session, crate::place::Heard)> {
        Err(crate::place::Refusal::Unsupported("a spawn that carries its first order"))
    }
    /// The answer to an order [`Host::spawn_ordered`] sent, waited for `wait` more.
    fn await_order(&self, _order: &crate::place::OrderId, _wait: Duration) -> Result<crate::place::Heard> {
        Err(crate::place::Refusal::Unsupported("an order's answer heard as an event"))
    }

    // ---- seat.stop / seat.stopped
    /// `TERM` to the group, `KILL` after the grace.
    fn stop(&self, stop: Stop<'_>);
    /// [`Host::stop`] of `seat`, and what ended: the manager's own `seat.stopped`
    /// (its `ended` pids and `exit`) where the host is the manager, the group
    /// this process signalled otherwise. A manager that does not run `seat`, or
    /// does not answer, is an error and never a stop that went well.
    fn end(&self, _seat: &str, stop: Stop<'_>) -> Result<Ended> {
        let group = stop.group;
        self.stop(stop);
        Ok(Ended { pids: vec![group], exit: None })
    }
    /// The group ended a moment from now, by a process that outlives this one.
    fn stop_soon(&self, group: u32);
    /// A socket nothing answers on any more, removed with its sidecars.
    fn clear_socket(&self, socket: &Path, sidecars: &[&str]);
    /// A local socket file removed, whether or not anything answers on it.
    fn unlink(&self, path: &Path);

    // ---- seats
    /// The pids of `pids` still running (a zombie is not).
    fn alive(&self, pids: &[u32]) -> BTreeSet<u32>;
    /// `root`, its descendants and its process group.
    fn tree_of(&self, root: u32) -> Vec<u32>;
    /// Whether something accepts on this socket.
    fn answers(&self, socket: &Path) -> bool;
    /// Whether this one pid is running: [`Host::alive`] for one.
    fn is_alive(&self, pid: u32) -> bool {
        self.alive(&[pid]).contains(&pid)
    }
    /// When `pid` started, as `ps -o lstart=` says it with its spaces written
    /// as `_`; `None` for no such process, `Err` where `ps` could not tell.
    fn started(&self, pid: u32) -> std::result::Result<Option<String>, ()>;
    /// The process table a seat's running shells are read from:
    /// `ps -A -o pid=,ppid=,etime=,time=,command=`. `None` when `ps` did not run.
    fn shells(&self) -> Option<String>;
    /// The process table per-seat memory is read from, environments included
    /// so a seat's id finds its processes: `ps -A -E -o pid=,ppid=,rss=,command=`.
    /// `None` when `ps` did not run or failed.
    fn seat_memory(&self) -> Option<String>;

    // ---- seats: the process table, signals and working directories
    // (wsp-008). Every `ps`, `kill` and `lsof` outside this module comes
    // through these; at M1 they become the `seats` reply, `seat.stop` and
    // `job.stop`, and at M3 the tree work's callers stop asking.
    /// `ps -A -o <columns>`, each line with its process's environment when
    /// `env` (`ps -E`). `None` when `ps` would not run or failed.
    fn table(&self, columns: &str, env: bool) -> Option<String>;
    /// `ps -o <columns> -p <pids>`: the rows of those still running. `None`
    /// when `ps` would not run.
    fn rows_of(&self, pids: &[u32], columns: &str) -> Option<String>;
    /// `kill -<sig>` to every pid of `pids` (`"0"` only asks), and whether it
    /// reached them all. `None` when `kill` would not run: "could not ask" is
    /// not "not there", and a caller that breaks something on the answer (a
    /// tree lock's dead holder) must keep it on `None`. An empty list sends
    /// nothing and answers `Some(false)`.
    fn signal(&self, pids: &[u32], sig: &str) -> Option<bool>;
    /// `sig` to a whole process group, by its leader's id — never this
    /// process's own group.
    fn signal_group(&self, group: u32, sig: &str);
    /// `lsof -d cwd -Fpn`, of `pids` alone when given: every process's working
    /// directory as lsof's field lines. `None` when `lsof` would not run.
    fn cwds(&self, pids: Option<&[u32]>) -> Option<String>;

    /// Whether `pid` is running: `kill -0`, which a zombie still answers.
    /// `None` when `kill` would not run, as [`Host::signal`].
    fn probe(&self, pid: u32) -> Option<bool> {
        match pid {
            0 => Some(false),
            _ => self.signal(&[pid], "0"),
        }
    }
    /// Every process as `(pid, ppid, the rest of the line)`, where the rest
    /// includes the environment.
    ///
    /// `ps -E` is how a process's environment is readable without a dependency
    /// or a `/proc`, and the environment is the only place a plugin's child says
    /// which backend started it: its command line is `wsp daemon`, identical to
    /// the live one's. `ppid` comes back in the same call so the ancestors of
    /// this process can be ruled out of anything about to be killed
    /// ([`wsp_base::base::procs::ancestors`]).
    fn processes(&self) -> Vec<(u32, u32, String)> {
        wsp_base::base::procs::read_processes(&self.table("pid=,ppid=,command=", true).unwrap_or_default())
    }
    /// Every process as `(pid, ppid, "")`: the parent links alone, for a caller
    /// that only walks `ancestors`. Without `-E` the table is a fifth of the
    /// work to read.
    fn parents(&self) -> Vec<(u32, u32, String)> {
        wsp_base::base::procs::read_parents(&self.table("pid=,ppid=", false).unwrap_or_default())
    }

    // ---- seat.read
    /// The renderer's screen, as bare text.
    fn read_screen(&self, render: &Path) -> Result<String>;
    /// The renderer's own reading of the screen for an agent kind.
    fn screen_state(&self, render: &Path, kind: &str) -> Option<State>;
    /// The child's last OSC title, off the durable half.
    fn osc_title(&self, socket: &Path) -> Option<String>;

    // ---- order.deliver / order.delivered / order.refused
    /// Whether a supervisor that can be asked to `deliver` is installed.
    fn can_deliver(&self) -> bool;
    /// `compound-sup deliver` under the id wsp minted, and its answer.
    fn deliver(&self, session: &Path, id: &str, text: &str, wait: Duration, seat: &Seat) -> Result<Delivery>;
    /// The text typed at the renderer, and Enter.
    fn type_in(&self, render: &Path, text: &str) -> Result<()>;
    /// Enter alone.
    fn press_enter(&self, render: &Path) -> Result<()>;
    /// One line appended to a headless seat's feed.
    fn append_line(&self, file: &Path, line: &str) -> Result<()>;

    // ---- seat.said / seat.spent
    /// The hooks' last word ([`SAID_FILE`]), or `{}`.
    fn said(&self, dir: &Path) -> Value;
    fn say(&self, dir: &Path, said: &Value);
    /// One line onto [`HOOKS_FILE`].
    fn log_hook(&self, dir: &Path, line: &Value);
    /// The newest write in a seat's directory.
    fn quiet_since(&self, dir: &Path) -> Option<i64>;
    /// This hook's slice of the transcript added to [`BURN_FILE`].
    fn tally(&self, dir: &Path, payload: &Value);
    /// The burn record banked in [`SPENT_FILE`] before the seat goes.
    fn bank(&self, dir: &Path);
    /// Every seat's burn record under `root`.
    fn burn_under(&self, root: &Path) -> Vec<(String, Value)>;

    // ---- seat.did: transcripts
    /// Where a Claude Code session was written, `seat.did`'s transcript ref:
    /// the file named after `cwd` first, else any project's file of that id.
    fn transcript_of(&self, session: &str, cwd: &str) -> Option<PathBuf>;
    /// Where a pi session under `cwd` was written, the newest file of that id.
    fn pi_session_of(&self, session: &str, cwd: &str) -> Option<PathBuf>;
    /// A transcript a line at a time; a transcript is as long as its session.
    fn read_transcript(&self, file: &Path) -> Option<Box<dyn BufRead>>;
    /// A transcript whole.
    fn transcript_bytes(&self, file: &Path) -> Option<Vec<u8>>;
    /// The last `bytes` of a transcript, from its first whole line.
    fn transcript_tail(&self, file: &Path, bytes: u64) -> Option<String>;
    /// `opencode export --sanitize` of one session: opencode keeps no file.
    fn opencode_export(&self, session: &str) -> Option<Vec<u8>>;
    /// The newest two messages of an opencode session (`role`, `created`,
    /// `completed`), newest first, off `opencode db`.
    fn opencode_newest(&self, session: &str) -> Option<Vec<Value>>;

    // ---- turn
    /// Whether [`HOOKS_FILE`] shows the seat's own prompt submitted at or
    /// after `since`; `None` when there is no log to say.
    fn turn_began(&self, dir: &Path, since: i64) -> Option<bool>;

    // ---- beat (host facts)
    // Each is the probe's own text; what it means is read by the caller's
    // parser, so a fact crosses as the same text at M1 until `HostFacts` carries it.
    /// `df -Pk <path>`, when it succeeded.
    fn disk_free(&self, path: &Path) -> Option<String>;
    /// `du -sk <path>`, when it ran.
    fn disk_use(&self, path: &Path) -> Option<String>;
    /// The load averages: `/proc/loadavg` where there is one, else
    /// `sysctl -n vm.loadavg`.
    fn load(&self) -> Option<String>;
    /// `pmset -g therm`, when it succeeded.
    fn thermal(&self) -> Option<String>;
    /// `memory_pressure`, when it succeeded.
    fn memory_pressure(&self) -> Option<String>;
    /// `sysctl -n vm.swapusage`, when it succeeded.
    fn swap(&self) -> Option<String>;
    /// `vm_stat`, when it succeeded.
    fn pageouts(&self) -> Option<String>;
    /// `ioreg -c IOHIDSystem -d 4`, when it succeeded: how long the person has
    /// been away from the keyboard.
    fn idle(&self) -> Option<String>;

    // ---- hello
    /// What the doctor says about the installed `compound-sup`'s wire.
    fn wire_health(&self, problems: &mut Vec<String>, notes: &mut Vec<String>);
}

/// The host this process is: `wsp-host`'s [`wsp_host::InProcess`], reached
/// only through [`Host`]. A newtype rather than the type itself, so a caller
/// still names the trait to ask it anything, as it did before the move, and a
/// second implementation (M1's `host::Tcp`) can stand where this one does.
pub struct InProcess(wsp_host::InProcess);

/// The in-process host every backend gets unless a test hands it another.
pub static LOCAL: InProcess = InProcess::new();

impl InProcess {
    pub const fn new() -> InProcess {
        InProcess(wsp_host::InProcess::new())
    }

    /// One whose `compound-sup` is this file — a test's script.
    pub fn with_sup(sup: &Path) -> InProcess {
        InProcess(wsp_host::InProcess::with_sup(sup))
    }
}

impl Default for InProcess {
    fn default() -> InProcess {
        InProcess::new()
    }
}

/// The adapter: each method is `wsp-host`'s in-process method of the same name.
impl Host for InProcess {
    fn spawn_session(&self, spawn: &Spawn<'_>) -> Result<Session> {
        self.0.spawn_session(spawn)
    }
    fn spawn_headless(&self, spawn: &Headless<'_>) -> Result<Forked> {
        self.0.spawn_headless(spawn)
    }
    fn stop(&self, stop: Stop<'_>) {
        self.0.stop(stop)
    }
    fn stop_soon(&self, group: u32) {
        self.0.stop_soon(group)
    }
    fn clear_socket(&self, socket: &Path, sidecars: &[&str]) {
        self.0.clear_socket(socket, sidecars)
    }
    fn unlink(&self, path: &Path) {
        self.0.unlink(path)
    }
    fn alive(&self, pids: &[u32]) -> BTreeSet<u32> {
        self.0.alive(pids)
    }
    fn started(&self, pid: u32) -> std::result::Result<Option<String>, ()> {
        self.0.started(pid)
    }
    fn shells(&self) -> Option<String> {
        self.0.shells()
    }
    fn seat_memory(&self) -> Option<String> {
        self.0.seat_memory()
    }
    fn disk_free(&self, path: &Path) -> Option<String> {
        self.0.disk_free(path)
    }
    fn disk_use(&self, path: &Path) -> Option<String> {
        self.0.disk_use(path)
    }
    fn load(&self) -> Option<String> {
        self.0.load()
    }
    fn thermal(&self) -> Option<String> {
        self.0.thermal()
    }
    fn memory_pressure(&self) -> Option<String> {
        self.0.memory_pressure()
    }
    fn swap(&self) -> Option<String> {
        self.0.swap()
    }
    fn pageouts(&self) -> Option<String> {
        self.0.pageouts()
    }
    fn idle(&self) -> Option<String> {
        self.0.idle()
    }
    fn tree_of(&self, root: u32) -> Vec<u32> {
        self.0.tree_of(root)
    }
    fn table(&self, columns: &str, env: bool) -> Option<String> {
        self.0.table(columns, env)
    }
    fn rows_of(&self, pids: &[u32], columns: &str) -> Option<String> {
        self.0.rows_of(pids, columns)
    }
    fn signal(&self, pids: &[u32], sig: &str) -> Option<bool> {
        self.0.signal(pids, sig)
    }
    fn signal_group(&self, group: u32, sig: &str) {
        self.0.signal_group(group, sig)
    }
    fn cwds(&self, pids: Option<&[u32]>) -> Option<String> {
        self.0.cwds(pids)
    }
    fn answers(&self, socket: &Path) -> bool {
        self.0.answers(socket)
    }
    fn read_screen(&self, render: &Path) -> Result<String> {
        self.0.read_screen(render)
    }
    fn screen_state(&self, render: &Path, kind: &str) -> Option<State> {
        self.0.screen_state(render, kind)
    }
    fn osc_title(&self, socket: &Path) -> Option<String> {
        self.0.osc_title(socket)
    }
    fn can_deliver(&self) -> bool {
        self.0.can_deliver()
    }
    fn deliver(&self, session: &Path, id: &str, text: &str, wait: Duration, seat: &Seat) -> Result<Delivery> {
        self.0.deliver(session, id, text, wait, seat)
    }
    fn type_in(&self, render: &Path, text: &str) -> Result<()> {
        self.0.type_in(render, text)
    }
    fn press_enter(&self, render: &Path) -> Result<()> {
        self.0.press_enter(render)
    }
    fn append_line(&self, file: &Path, line: &str) -> Result<()> {
        self.0.append_line(file, line)
    }
    fn said(&self, dir: &Path) -> Value {
        self.0.said(dir)
    }
    fn say(&self, dir: &Path, said: &Value) {
        self.0.say(dir, said)
    }
    fn log_hook(&self, dir: &Path, line: &Value) {
        self.0.log_hook(dir, line)
    }
    fn quiet_since(&self, dir: &Path) -> Option<i64> {
        self.0.quiet_since(dir)
    }
    fn tally(&self, dir: &Path, payload: &Value) {
        self.0.tally(dir, payload)
    }
    fn bank(&self, dir: &Path) {
        self.0.bank(dir)
    }
    fn burn_under(&self, root: &Path) -> Vec<(String, Value)> {
        self.0.burn_under(root)
    }
    fn transcript_of(&self, session: &str, cwd: &str) -> Option<PathBuf> {
        self.0.transcript_of(session, cwd)
    }
    fn pi_session_of(&self, session: &str, cwd: &str) -> Option<PathBuf> {
        self.0.pi_session_of(session, cwd)
    }
    fn read_transcript(&self, file: &Path) -> Option<Box<dyn BufRead>> {
        self.0.read_transcript(file)
    }
    fn transcript_bytes(&self, file: &Path) -> Option<Vec<u8>> {
        self.0.transcript_bytes(file)
    }
    fn transcript_tail(&self, file: &Path, bytes: u64) -> Option<String> {
        self.0.transcript_tail(file, bytes)
    }
    fn opencode_export(&self, session: &str) -> Option<Vec<u8>> {
        self.0.opencode_export(session)
    }
    fn opencode_newest(&self, session: &str) -> Option<Vec<Value>> {
        self.0.opencode_newest(session)
    }
    fn turn_began(&self, dir: &Path, since: i64) -> Option<bool> {
        self.0.turn_began(dir, since)
    }
    fn wire_health(&self, problems: &mut Vec<String>, notes: &mut Vec<String>) {
        self.0.wire_health(problems, notes)
    }
}
