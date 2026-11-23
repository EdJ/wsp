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
//! The `ps` and `kill` of seat work are there too, as `seats`'s process table
//! and signals (wsp-008). Tree work asks none of them since wsp-057: its
//! sweep, slot probes and pid start times are gone, every run is a job
//! ([`Host::run_job`]), and a holder's liveness is its job in a job table or
//! a lease it beats (`wsp_store::lease`), never a probed pid.
//!
//! Tree work's jobs (M3, wsp-051) are [`Host::run_job`], [`Host::stop_job`]
//! and [`Host::jobs`]: `job.run`, `job.stop` and `jobs` on [`Tcp`], and
//! `wsp-host`'s runner in this process on [`InProcess`]. `WSP_JOBS=inproc|manager`
//! chooses between them ([`choose_jobs`], [`jobs_chosen`]; `manager` by
//! default since wsp-056, `inproc` the rollback), apart from `WSP_HOST`. The callers move at M3's group 5, the
//! first `throttle run` (wsp-052), which also asks [`Host::job_host`] for the
//! name a job's place carries and [`Host::facts`] for the beat it decides on.
//!
//! Not in the host yet: the far machine's ssh (`far.rs`'s [`crate::far::Wire`], M2's subject), and the
//! doctor's checks of installed hook snippets, which read config rather than a
//! seat.

use std::collections::{BTreeMap, BTreeSet};
use std::io::BufRead;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Condvar, Mutex};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use serde_json::Value;

use crate::place::{Delivery, Result, Seat, State};

/// What goes with the in-process host (the spawn and stop types, the hook file
/// names, the transcript paths), named here as before the move. Its
/// `InProcess` is wrapped below, and the local type shadows the glob.
pub use wsp_host::inproc::*;

/// M1's second implementation, over the manager's connection (wsp-025), and
/// `WSP_HOST`'s choice between the two.
pub use crate::host_tcp::{choose, choose_jobs, chosen, jobs_chosen, record, record_of, Choice, HostError, JobsChoice, Subscription, Tcp};

/// What a tree job is, as `job.run` carries it, and what comes back
/// (`docs/agent-manager.md` 3.2): the trait's job methods take and give these,
/// so a call is its message under either host.
pub use wsp_host_api::{At, HostDir, HostId, JobAt, JobClass, JobEnded, JobKey, JobOutput, JobRow, JobRun, JobState, RowId, Tree};

/// How long a stopped job has between `TERM` and `KILL`: the manager's
/// default (`--job-grace`), kept the same in-process.
pub const JOB_GRACE: Duration = Duration::from_secs(5);
/// The most of a job's output handed to a sink at once, as the manager sends it.
const JOB_CHUNK: usize = 64 * 1024;

/// A job's key, built once per run by its caller and sent unchanged however
/// often the job is asked for: a retry, and a resend after a reopen, are the
/// same key, so the job runs once. `job` must be unique per run (wsp-040): a
/// reopened row starts its attempts again, so it carries what makes this run
/// this one (`verify:clippy@3`).
pub fn job_key(row: &str, attempt: u32, job: &str) -> JobKey {
    JobKey { row: RowId(row.to_string()), attempt, job: job.to_string() }
}

/// Why a job was not run, or its end not heard.
#[derive(Debug, Clone, PartialEq)]
pub enum JobError {
    /// The manager did not answer: "host silent". The job was not run in this
    /// process in its place.
    Silent(HostError),
    /// The host refused it, in the manager's words (`full`, `duplicate`,
    /// `no_tree`, `no_job`, `malformed`, `failed`); the in-process host
    /// refuses in the same words.
    Refused { why: wsp_host_api::Refusal, detail: String },
    /// Asked, and no end came within the wait.
    Unanswered(String),
}

impl std::fmt::Display for JobError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            JobError::Silent(e) => write!(f, "{e}"),
            JobError::Refused { why, detail } => write!(f, "the job was refused: {why:?} {detail}"),
            JobError::Unanswered(why) => write!(f, "{why}"),
        }
    }
}

impl std::error::Error for JobError {}

impl From<JobError> for crate::place::Refusal {
    fn from(e: JobError) -> crate::place::Refusal {
        match e {
            JobError::Silent(e) => crate::place::Refusal::from(e),
            other => crate::place::Refusal::Backend(other.to_string()),
        }
    }
}

fn refused(why: wsp_host_api::Refusal, detail: impl Into<String>) -> JobError {
    JobError::Refused { why, detail: detail.into() }
}

/// Where the in-process host keeps its jobs' files when it is not told:
/// `~/.local/state/wsp/inproc`, beside the manager's exec dir.
pub fn default_exec() -> PathBuf {
    crate::util::home().join(".local/state/wsp/inproc")
}

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
    /// What became of the agent [`Host::spawn_session`] started in `seat`,
    /// heard as the host's events within `wait`: `seat.started` was the
    /// spawn's own answer, so without `by_turn` it is ready; with it, ready is
    /// its first `turn` begun, and `seat.stopped` first is a start that did not
    /// survive (wsp-037). The in-process host has no events.
    fn await_up(&self, _seat: &str, _by_turn: bool, _wait: Duration) -> Result<crate::place::Up> {
        Err(crate::place::Refusal::Unsupported("a start heard as an event"))
    }

    // ---- seat.stop / seat.stopped
    /// `TERM` to the group, `KILL` after the grace.
    fn stop(&self, stop: Stop<'_>);
    /// [`Host::stop`] of `seat`, and what ended: the manager's own `seat.stopped`
    /// (its `ended` pids and `exit`) where the host is the manager, the group
    /// this process signalled otherwise. A manager that does not answer is an
    /// error and never a stop that went well; one that answers it does not run
    /// `seat` leaves it to the in-process stop.
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
    /// Every Claude Code transcript written in a tree whose path ends in
    /// `tree` (`/.worktrees/<row>`), whatever session wrote it.
    fn transcripts_of_tree(&self, tree: &str) -> Vec<PathBuf>;
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

    // ---- job.run / job.started / job.output / job.ended (M3, wsp-051)
    // `WSP_JOBS` chooses the host a job runs on ([`choose_jobs`]), apart from
    // `WSP_HOST`, so jobs roll back on their own.
    /// `job.run`, and the job's `job.ended` once it has ended: its exit, or
    /// that it timed out or hung, and its log. What it writes meanwhile goes
    /// to `sink` as `job.output`, in order. `run.key` is the caller's, built
    /// once per run ([`job_key`]): asked again, it is the same job, never a
    /// second run, and its end once it has one. A manager that does not answer
    /// is [`JobError::Silent`], and nothing runs in this process instead.
    fn run_job(&self, run: &JobRun, _sink: &mut dyn FnMut(&JobOutput)) -> std::result::Result<JobEnded, JobError> {
        Err(refused(wsp_host_api::Refusal::Unsupported, format!("this host runs no jobs ({})", run.key.job)))
    }
    /// `job.stop`: `TERM` to the job's own group, `KILL` once [`JOB_GRACE`]
    /// is out, and its `job.ended` within `wait`. A job that has ended is
    /// answered with its end; one this host never ran is refused `no_job`.
    fn stop_job(&self, key: &JobKey, _wait: Duration) -> std::result::Result<JobEnded, JobError> {
        Err(refused(wsp_host_api::Refusal::Unsupported, format!("this host runs no jobs ({})", key.job)))
    }
    /// `jobs`: the job table, running and ended.
    fn jobs(&self) -> std::result::Result<Vec<JobRow>, JobError> {
        Err(refused(wsp_host_api::Refusal::Unsupported, "this host runs no jobs"))
    }
    /// The host its jobs run on, as its `hello` names itself: what a job's
    /// `HostDir` names (M3 under T2, wsp-052). `None` where this host is not
    /// asked by name (the in-process runner places a `dir` on this machine).
    fn job_host(&self) -> Option<HostId> {
        None
    }
    /// The host's facts as its manager's last beat read them (`seats`'s reply
    /// carries them): what the throttle decides on under `WSP_JOBS=manager`
    /// (d1 Q11, wsp-052), in place of its own `sysctl`, `pmset`, `ioreg` and
    /// `vm_stat`. `None` with no beat yet, from a silent manager, and from the
    /// in-process host, whose caller reads its own probes as before.
    fn facts(&self) -> Option<wsp_host_api::HostFacts> {
        None
    }
}

/// The host this process is: `wsp-host`'s [`wsp_host::InProcess`], reached
/// only through [`Host`]. A newtype rather than the type itself, so a caller
/// still names the trait to ask it anything, as it did before the move, and a
/// second implementation (M1's `host::Tcp`) can stand where this one does.
///
/// It also runs tree jobs here (`WSP_JOBS=inproc`, M3's rollback; the
/// default is `manager` since wsp-056): `wsp-host`'s runner ([`wsp_host::jobs`]), the code the manager
/// runs them with, so a job's files, bounds and end are the same under both.
/// Its job table is this process's; a key that ran before in another process
/// is found by the runner's files under the exec dir and never run again.
pub struct InProcess {
    host: wsp_host::InProcess,
    /// Where the runner keeps a job's files (`<exec>/jobs/<key>.*`); `None`
    /// is [`default_exec`].
    exec: Option<PathBuf>,
    /// What a job's `dir` is under, as the manager's `--tree-root`; `None` is
    /// the home directory, the manager's default. `choose_jobs` gives it the
    /// root the install wrote, the one wsp places a job under.
    tree_root: Option<PathBuf>,
    jobs: Mutex<BTreeMap<JobKey, Arc<Slot>>>,
}

/// The in-process host every backend gets unless a test hands it another.
pub static LOCAL: InProcess = InProcess::new();

impl InProcess {
    pub const fn new() -> InProcess {
        InProcess { host: wsp_host::InProcess::new(), exec: None, tree_root: None, jobs: Mutex::new(BTreeMap::new()) }
    }

    /// One whose `compound-sup` is this file — a test's script.
    pub fn with_sup(sup: &Path) -> InProcess {
        InProcess { host: wsp_host::InProcess::with_sup(sup), ..InProcess::new() }
    }

    /// The same, its jobs' files under `exec` and their `dir`s under
    /// `tree_root`.
    pub fn with_jobs(self, exec: &Path, tree_root: &Path) -> InProcess {
        InProcess { exec: Some(exec.to_path_buf()), tree_root: Some(tree_root.to_path_buf()), ..self }
    }

    fn exec_dir(&self) -> PathBuf {
        self.exec.clone().unwrap_or_else(default_exec)
    }

    // ---- this process's own children (wsp-111): not a seat's, so not the
    // trait's and never a manager's; the daemon's reaper asks [`LOCAL`] and
    // hands the tables to `wsp_base::calls`.
    /// `ps -axo pid=,ppid=,command=` and that `ps`'s pid, for
    /// `wsp_base::calls::children_of` at a daemon's start.
    pub fn own_children(&self) -> Option<(u32, String)> {
        self.host.own_children()
    }
    /// `ps -axo pid=,ppid=,stat=,comm=`, for `wsp_base::calls::reap_defunct_with`.
    pub fn own_defunct(&self) -> Option<String> {
        self.host.own_defunct()
    }
    /// kill(2) with signal 0, asked without running `kill` (wsp-076).
    pub fn pid_runs(&self, pid: u32) -> bool {
        self.host.pid_runs(pid)
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
        self.host.spawn_session(spawn)
    }
    fn spawn_headless(&self, spawn: &Headless<'_>) -> Result<Forked> {
        self.host.spawn_headless(spawn)
    }
    fn stop(&self, stop: Stop<'_>) {
        self.host.stop(stop)
    }
    fn stop_soon(&self, group: u32) {
        self.host.stop_soon(group)
    }
    fn clear_socket(&self, socket: &Path, sidecars: &[&str]) {
        self.host.clear_socket(socket, sidecars)
    }
    fn unlink(&self, path: &Path) {
        self.host.unlink(path)
    }
    fn alive(&self, pids: &[u32]) -> BTreeSet<u32> {
        self.host.alive(pids)
    }
    fn shells(&self) -> Option<String> {
        self.host.shells()
    }
    fn seat_memory(&self) -> Option<String> {
        self.host.seat_memory()
    }
    fn disk_free(&self, path: &Path) -> Option<String> {
        self.host.disk_free(path)
    }
    fn disk_use(&self, path: &Path) -> Option<String> {
        self.host.disk_use(path)
    }
    fn load(&self) -> Option<String> {
        self.host.load()
    }
    fn thermal(&self) -> Option<String> {
        self.host.thermal()
    }
    fn memory_pressure(&self) -> Option<String> {
        self.host.memory_pressure()
    }
    fn swap(&self) -> Option<String> {
        self.host.swap()
    }
    fn pageouts(&self) -> Option<String> {
        self.host.pageouts()
    }
    fn idle(&self) -> Option<String> {
        self.host.idle()
    }
    fn tree_of(&self, root: u32) -> Vec<u32> {
        self.host.tree_of(root)
    }
    fn table(&self, columns: &str, env: bool) -> Option<String> {
        self.host.table(columns, env)
    }
    fn rows_of(&self, pids: &[u32], columns: &str) -> Option<String> {
        self.host.rows_of(pids, columns)
    }
    fn signal(&self, pids: &[u32], sig: &str) -> Option<bool> {
        self.host.signal(pids, sig)
    }
    fn signal_group(&self, group: u32, sig: &str) {
        self.host.signal_group(group, sig)
    }
    fn answers(&self, socket: &Path) -> bool {
        self.host.answers(socket)
    }
    fn read_screen(&self, render: &Path) -> Result<String> {
        self.host.read_screen(render)
    }
    fn screen_state(&self, render: &Path, kind: &str) -> Option<State> {
        self.host.screen_state(render, kind)
    }
    fn osc_title(&self, socket: &Path) -> Option<String> {
        self.host.osc_title(socket)
    }
    fn can_deliver(&self) -> bool {
        self.host.can_deliver()
    }
    fn deliver(&self, session: &Path, id: &str, text: &str, wait: Duration, seat: &Seat) -> Result<Delivery> {
        self.host.deliver(session, id, text, wait, seat)
    }
    fn type_in(&self, render: &Path, text: &str) -> Result<()> {
        self.host.type_in(render, text)
    }
    fn press_enter(&self, render: &Path) -> Result<()> {
        self.host.press_enter(render)
    }
    fn append_line(&self, file: &Path, line: &str) -> Result<()> {
        self.host.append_line(file, line)
    }
    fn said(&self, dir: &Path) -> Value {
        self.host.said(dir)
    }
    fn say(&self, dir: &Path, said: &Value) {
        self.host.say(dir, said)
    }
    fn log_hook(&self, dir: &Path, line: &Value) {
        self.host.log_hook(dir, line)
    }
    fn quiet_since(&self, dir: &Path) -> Option<i64> {
        self.host.quiet_since(dir)
    }
    fn tally(&self, dir: &Path, payload: &Value) {
        self.host.tally(dir, payload)
    }
    fn bank(&self, dir: &Path) {
        self.host.bank(dir)
    }
    fn burn_under(&self, root: &Path) -> Vec<(String, Value)> {
        self.host.burn_under(root)
    }
    fn transcript_of(&self, session: &str, cwd: &str) -> Option<PathBuf> {
        self.host.transcript_of(session, cwd)
    }
    fn transcripts_of_tree(&self, tree: &str) -> Vec<PathBuf> {
        self.host.transcripts_of_tree(tree)
    }
    fn pi_session_of(&self, session: &str, cwd: &str) -> Option<PathBuf> {
        self.host.pi_session_of(session, cwd)
    }
    fn read_transcript(&self, file: &Path) -> Option<Box<dyn BufRead>> {
        self.host.read_transcript(file)
    }
    fn transcript_bytes(&self, file: &Path) -> Option<Vec<u8>> {
        self.host.transcript_bytes(file)
    }
    fn transcript_tail(&self, file: &Path, bytes: u64) -> Option<String> {
        self.host.transcript_tail(file, bytes)
    }
    fn opencode_export(&self, session: &str) -> Option<Vec<u8>> {
        self.host.opencode_export(session)
    }
    fn opencode_newest(&self, session: &str) -> Option<Vec<Value>> {
        self.host.opencode_newest(session)
    }
    fn wire_health(&self, problems: &mut Vec<String>, notes: &mut Vec<String>) {
        self.host.wire_health(problems, notes)
    }

    fn run_job(&self, run: &JobRun, sink: &mut dyn FnMut(&JobOutput)) -> std::result::Result<JobEnded, JobError> {
        use wsp_host::jobs as runner;
        let (exec, file) = (self.exec_dir(), runner::file_key(&run.key));
        let host_id = host_of(&run.at);
        let mut table = self.jobs.lock().unwrap_or_else(|e| e.into_inner());
        if let Some(slot) = table.get(&run.key).cloned() {
            drop(table);
            if !slot.same_run(run) {
                return Err(refused(wsp_host_api::Refusal::Duplicate, format!("job {} was already run with other arguments", slot.name())));
            }
            // A retry: the same job, its end once it has one.
            return Ok(slot.wait(None).expect("waited without a bound"));
        }
        // Run before, here or by another process: its end, never a second run.
        let log = runner::log_path(&exec, &file);
        if let Some(end) = runner::read_end(&exec, &file) {
            return Ok(end.ended(run.key.clone(), host_id, &log));
        }
        let job = match runner::recorded(&exec, &file) {
            Some(rec) => match runner::adopt(&self.host, &exec, &file, rec.pid, &rec.started) {
                runner::Adopted::Running(job) => job,
                runner::Adopted::Ended(end) => return Ok(end.ended(run.key.clone(), host_id, &log)),
                runner::Adopted::Lost => return Ok(runner::End::default().ended(run.key.clone(), host_id, &log)),
            },
            None => {
                if run.argv.is_empty() {
                    return Err(refused(wsp_host_api::Refusal::Malformed, "a job needs an argv"));
                }
                let cwd = self.place(&run.at).map_err(|why| refused(wsp_host_api::Refusal::NoTree, why))?;
                let spec = runner::Spec {
                    key: &file,
                    argv: &run.argv,
                    cwd: &cwd,
                    timeout: Duration::from_secs(run.timeout_secs),
                    hung_after: run.hung_after_secs.map(Duration::from_secs),
                };
                runner::start(&self.host, &exec, &spec)
                    .map_err(|e| refused(wsp_host_api::Refusal::Failed, format!("the job could not be started: {e}")))?
            }
        };
        let slot = Arc::new(Slot {
            run: run.clone(),
            pgid: job.pgid(),
            started: At(SystemTime::now().duration_since(UNIX_EPOCH).map_or(0, |d| d.as_secs())),
            stop: AtomicBool::new(false),
            end: Mutex::new(None),
            ended: Condvar::new(),
        });
        table.insert(run.key.clone(), slot.clone());
        drop(table);
        Ok(self.watch(&slot, job, host_id, sink))
    }
    fn stop_job(&self, key: &JobKey, wait: Duration) -> std::result::Result<JobEnded, JobError> {
        use wsp_host::jobs as runner;
        let slot = self.jobs.lock().unwrap_or_else(|e| e.into_inner()).get(key).cloned();
        if let Some(slot) = slot {
            slot.stop.store(true, Ordering::SeqCst);
            return slot.wait(Some(wait)).ok_or_else(|| {
                JobError::Unanswered(format!("job {} did not end within {}s of its stop", slot.name(), wait.as_secs()))
            });
        }
        // Not this process's: what the runner's files say of it.
        let (exec, file) = (self.exec_dir(), runner::file_key(key));
        let log = runner::log_path(&exec, &file);
        let here = HostId(String::new());
        if let Some(end) = runner::read_end(&exec, &file) {
            return Ok(end.ended(key.clone(), here, &log));
        }
        let Some(rec) = runner::recorded(&exec, &file) else {
            return Err(refused(wsp_host_api::Refusal::NoJob, format!("this host runs no job {}", file)));
        };
        let end = match runner::adopt(&self.host, &exec, &file, rec.pid, &rec.started) {
            runner::Adopted::Running(mut job) => self.end_group(&mut job),
            runner::Adopted::Ended(end) => end,
            runner::Adopted::Lost => runner::End::default(),
        };
        Ok(end.ended(key.clone(), here, &log))
    }
    fn jobs(&self) -> std::result::Result<Vec<JobRow>, JobError> {
        let table = self.jobs.lock().unwrap_or_else(|e| e.into_inner());
        Ok(table
            .values()
            .map(|s| JobRow {
                key: s.run.key.clone(),
                class: s.run.class,
                pgid: s.pgid,
                started: s.started,
                state: match s.end.lock().unwrap_or_else(|e| e.into_inner()).is_some() {
                    true => JobState::Ended,
                    false => JobState::Running,
                },
            })
            .collect())
    }
}

/// A job this process runs, in its table: what was asked, and its end once
/// it has one, for a retry or a stop to wait on.
struct Slot {
    run: JobRun,
    pgid: u32,
    started: At,
    stop: AtomicBool,
    end: Mutex<Option<JobEnded>>,
    ended: Condvar,
}

impl Slot {
    fn name(&self) -> String {
        wsp_host::jobs::file_key(&self.run.key)
    }

    /// Whether `other` asks for the same run: a retry, not another job.
    fn same_run(&self, other: &JobRun) -> bool {
        self.run.class == other.class && self.run.argv == other.argv && self.run.at == other.at
    }

    /// The job's end, waited for up to `wait` (for ever when `None`).
    fn wait(&self, wait: Option<Duration>) -> Option<JobEnded> {
        let deadline = wait.and_then(|w| Instant::now().checked_add(w));
        let mut end = self.end.lock().unwrap_or_else(|e| e.into_inner());
        loop {
            if let Some(e) = end.as_ref() {
                return Some(e.clone());
            }
            end = match deadline {
                None => self.ended.wait(end).unwrap_or_else(|e| e.into_inner()),
                Some(d) => {
                    let left = d.saturating_duration_since(Instant::now());
                    if left.is_zero() {
                        return None;
                    }
                    self.ended.wait_timeout(end, left).unwrap_or_else(|e| e.into_inner()).0
                }
            };
        }
    }
}

/// The host a job's place names, which its log is named on.
fn host_of(at: &JobAt) -> HostId {
    match at {
        JobAt::Tree(t) => t.host.clone(),
        JobAt::Dir(d) => d.host.clone(),
    }
}

impl InProcess {
    /// The directory a job runs in, or why it may not: the manager's rule
    /// (`no_tree`), with this process's tree root. A row's `tree` is not
    /// placed until Q6, as the manager does not place it.
    fn place(&self, at: &JobAt) -> std::result::Result<PathBuf, String> {
        let dir = match at {
            JobAt::Tree(t) => return Err(format!("row {}'s tree is not placed until Q6 (T1 holds); name it as a dir under the tree root", t.row.0)),
            JobAt::Dir(d) => d,
        };
        if dir.escapes() {
            return Err(format!("{:?} falls outside the tree root", dir.path));
        }
        let root = self.tree_root.clone().unwrap_or_else(crate::util::home);
        let root = root.canonicalize().map_err(|e| format!("the tree root {} cannot be read: {e}", root.display()))?;
        let path = root.join(&dir.path).canonicalize().map_err(|e| format!("{:?} under the tree root: {e}", dir.path))?;
        if !path.starts_with(&root) {
            return Err(format!("{:?} leads outside the tree root", dir.path));
        }
        if !path.is_dir() {
            return Err(format!("{:?} is not a directory", dir.path));
        }
        Ok(path)
    }

    /// Watch `job` to its end, as the manager's watcher does: its output to
    /// `sink`, a stop carried out, and its end handed to the slot's waiters.
    fn watch(&self, slot: &Slot, mut job: wsp_host::jobs::Job, host_id: HostId, sink: &mut dyn FnMut(&JobOutput)) -> JobEnded {
        let key = &slot.run.key;
        let mut from = 0u64;
        let end = loop {
            from = hand_on(&job, key, from, sink);
            if let Some(end) = job.poll(&self.host) {
                break end;
            }
            if slot.stop.load(Ordering::SeqCst) {
                break self.end_group(&mut job);
            }
            std::thread::sleep(Duration::from_millis(100));
        };
        hand_on(&job, key, from, sink);
        let ended = end.ended(key.clone(), host_id, &job.log);
        *slot.end.lock().unwrap_or_else(|e| e.into_inner()) = Some(ended.clone());
        slot.ended.notify_all();
        ended
    }

    /// `job.stop`'s end: `TERM` to the job's own group, then `KILL` once
    /// [`JOB_GRACE`] is out.
    fn end_group(&self, job: &mut wsp_host::jobs::Job) -> wsp_host::jobs::End {
        self.host.signal_group(job.pgid(), "TERM");
        let until = Instant::now() + JOB_GRACE;
        while Instant::now() < until {
            if let Some(end) = job.poll(&self.host) {
                return wsp_host::jobs::End { stopped: true, ..end };
            }
            std::thread::sleep(Duration::from_millis(50));
        }
        job.stop(&self.host)
    }
}

/// What `job` wrote since `from`, handed to `sink` as `job.output`; the
/// offset it got to.
fn hand_on(job: &wsp_host::jobs::Job, key: &JobKey, mut from: u64, sink: &mut dyn FnMut(&JobOutput)) -> u64 {
    loop {
        let Ok((text, next)) = job.output(from, JOB_CHUNK) else { return from };
        if next == from {
            return from;
        }
        sink(&JobOutput { key: key.clone(), from, text });
        from = next;
    }
}
