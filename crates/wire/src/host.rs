//! The host seam: every OS operation a seat needs, behind one trait
//! (`docs/agent-manager.md` section 6.2, step M0).
//!
//! [`Host`] is what [`crate::place_compound`] and [`crate::place_super`] reach
//! the operating system through: spawning a seat's processes, stopping them,
//! its pids and process tree, the screen and state its renderer reports,
//! delivering an order, and the hook files and spend tallies its agent's hooks
//! write. Those are the OS halves of the two backends (section 2.2). The
//! *decisions* stay with the backends: which seat, what to run in it, when a
//! hook's word has aged out, whether a seat will take a prompt, what a seat
//! record says.
//!
//! The methods are grouped by the host-api message they become at M1
//! (section 3.2): `seat.spawn`, `seat.stop`, `seats`, `seat.read`,
//! `order.deliver`, `seat.said`/`seat.spent` and `hello`. Each takes plain
//! values (paths, pids, ids, text), never a seat record, so a call turns into a
//! message without a wsp type crossing the wire.
//!
//! [`InProcess`] is the only implementation: today's code, moved here whole,
//! run in this process. At M1 the manager takes this module and a second
//! implementation sends the same calls as messages.
//!
//! The `ps`, `kill` and `lsof` of seat and tree work are here too, as `seats`'s
//! process table, signals and working directories (wsp-008). Tree work's
//! check_tree sweep, the throttle's and gc's liveness probes and a
//! check's kill on timeout ask [`LOCAL`], so at M3 their jobs move and their
//! calls do not.
//!
//! Not here yet: tree work's jobs (section 1.3, which stays wsp's), the far machine's
//! ssh (`far.rs`'s [`crate::far::Wire`], M2's subject), and the doctor's
//! checks of installed hook snippets, which read config rather than a seat.

#![allow(dead_code)]

use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::io::{BufRead, BufReader, Write};
use std::net::Shutdown;
use std::os::unix::net::UnixStream;
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::sync::mpsc;
use std::time::{Duration, Instant};
use wsp_base::calls::Command;

use serde_json::{json, Value};

use crate::place::{self, Delivery, Refusal, Result, Seat, State};
use crate::place_compound::{reads_kind, WIRE_VERSION};
use crate::place_super::{cost, cost_of, read_json, str_of};
use crate::store::write_atomic;
use crate::util::{self, Clock};

/// What the agent's hooks last said about it, one record, rewritten whole.
pub const SAID_FILE: &str = "said.json";
/// Every hook a compound seat has been heard from, one JSON line each.
pub const HOOKS_FILE: &str = "hooks.jsonl";
/// What the session has cost so far, tallied from its transcript.
pub const BURN_FILE: &str = "burn.json";

/// How long `spawn_session` waits for `compound-sup`'s handshake.
const HANDSHAKE_TIMEOUT: Duration = Duration::from_secs(5);
/// How long a dial to a session's socket waits for it to accept.
pub(crate) const DIAL_TIMEOUT: Duration = Duration::from_secs(2);

/// `seat.spawn` for a pty seat: what runs in it and where, already decided.
pub struct Spawn<'a> {
    /// The seat's id, written as the session's label.
    pub seat: &'a str,
    /// The agent kind the supervisor reads a composer for, when it reads one.
    pub kind: Option<String>,
    pub cwd: String,
    /// Set over the supervisor's own environment; an empty value removes.
    pub env: BTreeMap<String, String>,
    pub program: &'a str,
    pub args: &'a [String],
}

/// `seat.started` for a pty seat: the supervisor's pid and its two sockets.
pub struct Session {
    pub pid: u32,
    pub socket: PathBuf,
    pub render: Option<PathBuf>,
}

/// `seat.spawn` for a headless seat: an agent with a `tail -f` on its stdin.
pub struct Headless<'a> {
    pub seat: &'a str,
    pub program: String,
    pub args: Vec<String>,
    pub cwd: String,
    pub env: BTreeMap<String, String>,
    /// The file the feed follows, and the agent's two streams.
    pub prompts: PathBuf,
    pub out: PathBuf,
    pub err: PathBuf,
}

/// `seat.started` for a headless seat: the agent and the group it runs in.
pub struct Forked {
    pub pid: u32,
    pub group: u32,
}

/// `seat.stop`: the group to end, the pid whose exit says it ended, and the
/// grace between `TERM` and `KILL`. The clock is the caller's, so a test's
/// fake time stops a seat the same way.
pub struct Stop<'a> {
    pub group: u32,
    pub watch: Option<u32>,
    pub grace: Duration,
    pub poll: Duration,
    pub clock: &'a dyn Clock,
}

/// Every OS operation a seat needs, grouped by the host-api message it
/// becomes (`docs/agent-manager.md` section 3.2).
pub trait Host: Sync {
    // ---- seat.spawn / seat.started
    /// `compound-sup spawn`, its handshake read.
    fn spawn_session(&self, spawn: &Spawn<'_>) -> Result<Session>;
    /// A headless agent forked with its feed, in a group of its own.
    fn spawn_headless(&self, spawn: &Headless<'_>) -> Result<Forked>;

    // ---- seat.stop / seat.stopped
    /// `TERM` to the group, `KILL` after the grace.
    fn stop(&self, stop: Stop<'_>);
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
    fn hook_log(&self, dir: &Path) -> Option<String>;
    /// The newest write in a seat's directory.
    fn quiet_since(&self, dir: &Path) -> Option<i64>;
    /// This hook's slice of the transcript added to [`BURN_FILE`].
    fn tally(&self, dir: &Path, payload: &Value);
    /// The burn record banked in [`SPENT_FILE`] before the seat goes.
    fn bank(&self, dir: &Path);
    /// Every seat's burn record under `root`.
    fn burn_under(&self, root: &Path) -> Vec<(String, Value)>;

    // ---- hello
    /// What the doctor says about the installed `compound-sup`'s wire.
    fn wire_health(&self, problems: &mut Vec<String>, notes: &mut Vec<String>);
}

/// The host this process is: today's code, run here.
pub struct InProcess {
    /// A `compound-sup` named by a test; `None` finds the installed one.
    sup: Option<PathBuf>,
}

/// The in-process host every backend gets unless a test hands it another.
pub static LOCAL: InProcess = InProcess::new();

impl InProcess {
    pub const fn new() -> InProcess {
        InProcess { sup: None }
    }

    /// One whose `compound-sup` is this file — a test's script.
    pub fn with_sup(sup: &Path) -> InProcess {
        InProcess { sup: Some(sup.to_path_buf()) }
    }

    fn sup(&self) -> Option<PathBuf> {
        self.sup.clone().or_else(compound_sup_binary)
    }
}

impl Default for InProcess {
    fn default() -> InProcess {
        InProcess::new()
    }
}

impl Host for InProcess {
    fn spawn_session(&self, spawn: &Spawn<'_>) -> Result<Session> {
        let Some(sup) = self.sup() else {
            return Err(Refusal::Backend("compound-sup not found — set $COMPOUND_SUP or put it on PATH".into()));
        };
        let mut cmd = sup_command(&sup, spawn);
        let mut child = cmd.spawn().map_err(|e| Refusal::Backend(format!("{} did not start: {e}", sup.display())))?;
        let pid = child.id();
        #[cfg(any(test, feature = "test-support"))]
        crate::place_compound::test_sups::note(pid, &Seat::new(spawn.seat));
        let Some(stdout) = child.stdout.take() else {
            let _ = child.kill();
            return Err(Refusal::Backend("compound-sup gave no handshake pipe".into()));
        };
        // The child is daemonized (setsid) and outlives this process by
        // design — dropped rather than waited on, same as
        // `crates/host/src/sessions.rs::Panes::open`.
        drop(child);
        let Some((socket, render)) = read_handshake(stdout, HANDSHAKE_TIMEOUT) else {
            signal_group(pid, "KILL");
            return Err(Refusal::Backend("compound-sup never announced a socket".into()));
        };
        Ok(Session { pid, socket, render })
    }

    fn spawn_headless(&self, spawn: &Headless<'_>) -> Result<Forked> {
        let io = |path: &Path| -> Result<fs::File> {
            fs::OpenOptions::new()
                .create(true)
                .append(true)
                .open(path)
                .map_err(|e| Refusal::Backend(e.to_string()))
        };
        io(&spawn.prompts)?;
        let out = io(&spawn.out)?;
        let err = io(&spawn.err)?;

        // The durable writer on the agent's stdin. `-n +1` so that anything
        // already in the file is delivered rather than skipped, which matters
        // only for a seat being restarted and costs nothing when it is not.
        let mut feeder = Command::new("tail")
            .args(["-n", "+1", "-f"])
            .arg(&spawn.prompts)
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .process_group(0)
            .spawn()
            .map_err(|e| Refusal::Backend(format!("no way to feed the agent: {e}")))?;
        let group = feeder.id();
        let Some(feed) = feeder.stdout.take() else {
            signal_group(group, "KILL");
            return Err(Refusal::Backend("the feed had no pipe".into()));
        };

        let mut cmd = Command::new(&spawn.program);
        cmd.args(&spawn.args)
            .stdin(Stdio::from(feed))
            .stdout(Stdio::from(out))
            .stderr(Stdio::from(err))
            .process_group(group as i32);
        if !spawn.cwd.is_empty() {
            cmd.current_dir(&spawn.cwd);
        }
        child_env(&mut cmd, &Seat::new(spawn.seat), &spawn.env);

        match cmd.spawn() {
            Ok(child) => Ok(Forked { pid: child.id(), group }),
            Err(e) => {
                // The feed is not left running against a seat with no agent in
                // it: a `tail -f` nobody is reading is a leak that would outlive
                // every failed spawn.
                signal_group(group, "KILL");
                Err(Refusal::Backend(format!("{} did not start: {e}", spawn.program)))
            }
        }
    }

    fn stop(&self, stop: Stop<'_>) {
        signal_group(stop.group, "TERM");
        let Some(pid) = stop.watch else { return };
        let deadline = stop.clock.now() + stop.grace;
        while util::alive(&[pid]).contains(&pid) {
            if stop.clock.now() >= deadline {
                signal_group(stop.group, "KILL");
                break;
            }
            stop.clock.rest(stop.poll);
        }
    }

    fn stop_soon(&self, group: u32) {
        close_pane_soon(group)
    }

    fn clear_socket(&self, socket: &Path, sidecars: &[&str]) {
        // `compound-sup` unlinks its own socket on the way out of its run
        // loop, but installs no `SIGTERM` handler — measured, not assumed —
        // so a signalled exit skips that cleanup and leaves the headstone
        // `compound-028` d2 already named the remedy for: connect, and an
        // `ECONNREFUSED` says nothing is listening, which is what licenses
        // removing the file rather than a process.
        if socket.exists() && UnixStream::connect(socket).is_err() {
            let _ = fs::remove_file(socket);
            for ext in sidecars {
                let _ = fs::remove_file(socket.with_extension(ext));
            }
        }
    }

    fn unlink(&self, path: &Path) {
        let _ = fs::remove_file(path);
    }

    fn alive(&self, pids: &[u32]) -> BTreeSet<u32> {
        util::alive(pids)
    }

    fn tree_of(&self, root: u32) -> Vec<u32> {
        tree_of(root)
    }

    fn table(&self, columns: &str, env: bool) -> Option<String> {
        let mut cmd = Command::new("ps");
        cmd.arg("-A");
        if env {
            cmd.arg("-E");
        }
        let out = cmd.args(["-o", columns]).stdin(Stdio::null()).stderr(Stdio::null()).output().ok()?;
        out.status.success().then(|| String::from_utf8_lossy(&out.stdout).into_owned())
    }

    fn rows_of(&self, pids: &[u32], columns: &str) -> Option<String> {
        if pids.is_empty() {
            return Some(String::new());
        }
        let list = pids.iter().map(u32::to_string).collect::<Vec<_>>().join(",");
        // `ps -p` fails when any pid has gone and still lists the rest.
        let out = Command::new("ps").args(["-o", columns, "-p", &list]).stdin(Stdio::null()).stderr(Stdio::null()).output().ok()?;
        Some(String::from_utf8_lossy(&out.stdout).into_owned())
    }

    fn signal(&self, pids: &[u32], sig: &str) -> Option<bool> {
        if pids.is_empty() {
            return Some(false);
        }
        let status = Command::new("kill")
            .arg(format!("-{sig}"))
            .args(pids.iter().map(u32::to_string))
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status()
            .ok()?;
        Some(status.success())
    }

    fn signal_group(&self, group: u32, sig: &str) {
        signal_group(group, sig)
    }

    fn cwds(&self, pids: Option<&[u32]>) -> Option<String> {
        let mut cmd = Command::new("lsof");
        match pids {
            Some(pids) => {
                let list = pids.iter().map(u32::to_string).collect::<Vec<_>>().join(",");
                cmd.args(["-a", "-d", "cwd", "-Fpn", "-p", &list])
            }
            None => cmd.args(["-d", "cwd", "-Fpn"]),
        };
        let out = cmd.stdin(Stdio::null()).stderr(Stdio::null()).output().ok()?;
        Some(String::from_utf8_lossy(&out.stdout).into_owned())
    }

    fn answers(&self, socket: &Path) -> bool {
        UnixStream::connect(socket).is_ok()
    }

    fn read_screen(&self, render: &Path) -> Result<String> {
        let bin = compound_render_binary().ok_or_else(|| {
            Refusal::Backend("compound-render not found — set $COMPOUND_RENDER or put it on PATH".into())
        })?;
        let out = Command::new(&bin)
            .args(["screen"])
            .arg(render)
            .output()
            .map_err(|e| Refusal::Backend(e.to_string()))?;
        if !out.status.success() {
            return Err(Refusal::Backend(String::from_utf8_lossy(&out.stderr).trim().to_string()));
        }
        let raw = String::from_utf8_lossy(&out.stdout);
        let mut lines: Vec<&str> = raw.lines().collect();
        if lines.last().is_some_and(|l| l.starts_with("osc_title ")) {
            lines.pop();
        }
        if lines.last().is_some_and(|l| l.starts_with("size ")) {
            lines.pop();
        }
        Ok(lines
            .into_iter()
            .map(|l| l.split_once('|').map_or(l, |(_, t)| t))
            .collect::<Vec<_>>()
            .join("\n"))
    }

    fn screen_state(&self, render: &Path, kind: &str) -> Option<State> {
        let bin = compound_render_binary()?;
        let out = Command::new(&bin).args(["state"]).arg(render).arg(kind).output().ok()?;
        if !out.status.success() {
            return None;
        }
        match String::from_utf8_lossy(&out.stdout).lines().next()? {
            "idle" => Some(State::Idle),
            "working" => Some(State::Working),
            "blocked" => Some(State::Blocked),
            // "unknown", or a shape this backend does not recognise yet —
            // read the same as no answer at all rather than guessed at.
            _ => Some(State::Unknown),
        }
    }

    fn osc_title(&self, socket: &Path) -> Option<String> {
        let sup = self.sup()?;
        let out = Command::new(&sup).args(["vitals"]).arg(socket).output().ok()?;
        if !out.status.success() {
            return None;
        }
        String::from_utf8_lossy(&out.stdout).lines().find_map(|l| l.strip_prefix("osc_title ").map(str::to_string))
    }

    fn can_deliver(&self) -> bool {
        self.sup().is_some()
    }

    fn deliver(&self, session: &Path, id: &str, text: &str, wait: Duration, seat: &Seat) -> Result<Delivery> {
        let sup = self
            .sup()
            .ok_or_else(|| Refusal::Backend("compound-sup not found — set $COMPOUND_SUP or put it on PATH".into()))?;
        deliver_with(&sup, session, id, text, wait, seat)
    }

    fn type_in(&self, render: &Path, text: &str) -> Result<()> {
        type_and_submit(render, text)
    }

    fn press_enter(&self, render: &Path) -> Result<()> {
        let mut stream = dial(render, DIAL_TIMEOUT)?;
        press_enter(&mut stream)?;
        let _ = stream.shutdown(Shutdown::Both);
        Ok(())
    }

    fn append_line(&self, file: &Path, line: &str) -> Result<()> {
        let mut f = fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(file)
            .map_err(|e| Refusal::Backend(e.to_string()))?;
        // One `write_all` of one line, which is what makes this safe against a
        // second `tell` arriving at the same moment: `O_APPEND` is atomic for a
        // write this size, so two sentences interleave as two lines rather than
        // as one unparseable one.
        writeln!(f, "{line}").map_err(|e| Refusal::Backend(e.to_string()))
    }

    fn said(&self, dir: &Path) -> Value {
        read_json(&dir.join(SAID_FILE))
    }

    fn say(&self, dir: &Path, said: &Value) {
        let _ = write_atomic(&dir.join(SAID_FILE), &said.to_string());
    }

    fn log_hook(&self, dir: &Path, line: &Value) {
        // Best-effort, like everything else a hook touches — a write that
        // fails here must not be the write that fails the hook.
        if let Ok(mut f) = fs::OpenOptions::new().create(true).append(true).open(dir.join(HOOKS_FILE)) {
            let _ = writeln!(f, "{line}");
        }
    }

    fn hook_log(&self, dir: &Path) -> Option<String> {
        fs::read_to_string(dir.join(HOOKS_FILE)).ok()
    }

    fn quiet_since(&self, dir: &Path) -> Option<i64> {
        last_written(dir)
    }

    fn tally(&self, dir: &Path, payload: &Value) {
        tally_burn(&dir.to_path_buf(), payload)
    }

    fn bank(&self, dir: &Path) {
        bank_burn(dir)
    }

    fn burn_under(&self, root: &Path) -> Vec<(String, Value)> {
        burn_under(root)
    }

    fn wire_health(&self, problems: &mut Vec<String>, notes: &mut Vec<String>) {
        let Some(sup) = self.sup() else {
            notes.push(
                "compound-sup not found ($COMPOUND_SUP or PATH) — nothing to check about the compound wire".into(),
            );
            return;
        };
        wire_health_of(&sup, problems, notes);
    }
}

/// Where `compound-sup` lives: `$COMPOUND_SUP` when it names a file, else
/// `PATH`, else `~/.local/bin/compound-sup`, else beside the running `wsp`.
///
/// A set-but-missing `$COMPOUND_SUP` falls through rather than failing, and a
/// caller whose `PATH` lacks `~/.local/bin` (a seat's shell running `advance`)
/// still finds the binary the daemon's own `PATH` has (`wsp-602`).
pub(crate) fn compound_sup_binary() -> Option<PathBuf> {
    let exe_dir = std::env::current_exe().ok().and_then(|e| e.parent().map(PathBuf::from));
    find_sup(
        std::env::var_os("COMPOUND_SUP"),
        std::env::var_os("PATH"),
        std::env::var_os("HOME").map(PathBuf::from),
        exe_dir,
    )
}

pub(crate) fn find_sup(
    var: Option<std::ffi::OsString>,
    path: Option<std::ffi::OsString>,
    home: Option<PathBuf>,
    exe_dir: Option<PathBuf>,
) -> Option<PathBuf> {
    if let Some(p) = var.map(PathBuf::from).filter(|p| p.is_file()) {
        return Some(p);
    }
    if let Some(path) = path {
        if let Some(p) = std::env::split_paths(&path).map(|d| d.join("compound-sup")).find(|p| p.is_file()) {
            return Some(p);
        }
    }
    let local = home.filter(|h| !h.as_os_str().is_empty()).map(|h| h.join(".local/bin/compound-sup"));
    local.into_iter().chain(exe_dir.map(|d| d.join("compound-sup"))).find(|p| p.is_file())
}

#[cfg(test)]
thread_local! {
    /// A stand-in `compound-render` for this thread's calls to
    /// [`compound_render_binary`].
    pub(crate) static RENDER: std::cell::RefCell<Option<PathBuf>> = const { std::cell::RefCell::new(None) };
}

/// Where `compound-render` lives: `$COMPOUND_RENDER`, `PATH`, or beside
/// whichever `compound-sup` this process is already using.
///
/// The third door matters more than it looks (`compound-136`): the two
/// binaries are always built together into one `cargo build --target-dir`
/// (`compound-sup`'s own `renderer_binary_path` takes the identical
/// shortcut, one process over — they land beside each other by construction,
/// not by convention this file invented), so an operator or a test that has
/// only ever had to point wsp at `$COMPOUND_SUP` keeps working without
/// learning a second variable.
pub(crate) fn compound_render_binary() -> Option<PathBuf> {
    // A test's stand-in, per thread so no other test meets it.
    #[cfg(test)]
    if let Some(p) = RENDER.with(|r| r.borrow().clone()) {
        return Some(p);
    }
    if let Some(p) = std::env::var_os("COMPOUND_RENDER") {
        let p = PathBuf::from(p);
        return p.is_file().then_some(p);
    }
    if let Some(path) = std::env::var_os("PATH") {
        if let Some(p) = std::env::split_paths(&path).map(|d| d.join("compound-render")).find(|p| p.is_file()) {
            return Some(p);
        }
    }
    compound_sup_binary()?.parent().map(|d| d.join("compound-render")).filter(|p| p.is_file())
}
/// `compound-sup`'s four-line handshake since `compound-101`'s split — `sup
/// <pid>`, `pid <pid>`, `socket <path>`, `render <path>` — read with a
/// deadline off a background thread so a supervisor that never prints one (a
/// bad build, a hung `zig` toolchain, anything short of the ordinary path)
/// cannot hang [`Place::start`] rather than refuse it.
///
/// Only `socket` and `render` are parsed; `sup`'s pid is `Command::spawn`'s
/// own answer already, and `pid` is read from the record this same call
/// writes. Reads up to four lines rather than stopping the instant `render`
/// is seen, so an even older, pre-split `compound-sup` — three lines, no
/// `render` — still hands back its socket rather than timing out waiting for
/// a fourth line that binary will never print.
pub(crate) fn read_handshake(stdout: std::process::ChildStdout, timeout: Duration) -> Option<(PathBuf, Option<PathBuf>)> {
    let (tx, rx) = mpsc::channel();
    std::thread::spawn(move || {
        let mut reader = BufReader::new(stdout);
        let mut socket = None;
        let mut render = None;
        for _ in 0..4 {
            let mut line = String::new();
            if reader.read_line(&mut line).unwrap_or(0) == 0 {
                break;
            }
            let line = line.trim();
            if let Some(path) = line.strip_prefix("socket ") {
                socket = Some(PathBuf::from(path));
            } else if let Some(path) = line.strip_prefix("render ") {
                render = Some(PathBuf::from(path));
                break;
            }
        }
        // Best effort: a receiver that has already timed out is a send
        // nobody reads, not an error.
        let _ = tx.send(socket.map(|s| (s, render)));
    });
    rx.recv_timeout(timeout).ok().flatten()
}
/// [`wire_health`], given the binary rather than finding it — split out so a
/// test can point this at a fake script instead of racing every other test in
/// this process over `$COMPOUND_SUP` (`wsp-env-breaks-cargo-test`'s lesson,
/// one door over: a global is a global whether it names a store or a binary).
pub(crate) fn wire_health_of(sup: &std::path::Path, problems: &mut Vec<String>, notes: &mut Vec<String>) {
    // Follow the PATH symlink to the real binary, whose directory is the
    // cargo workspace's own `target/<profile>/` — where `host` lands too,
    // built by the same `cargo build` that builds this one. Not a path this
    // file invented: `scripts/supervisor_outlives_host_rebuild.sh` in the
    // compound repository already names both binaries at that convention.
    let real = fs::canonicalize(sup).unwrap_or_else(|_| sup.to_path_buf());
    if let Some(dir) = real.parent() {
        let host = dir.join("host");
        if let (Ok(hm), Ok(sm)) =
            (fs::metadata(&host).and_then(|m| m.modified()), fs::metadata(&real).and_then(|m| m.modified()))
        {
            if hm > sm {
                problems.push(format!(
                    "{} is newer than {} — a host-only rebuild (`cargo run -p host`) leaves compound-sup stale, \
                     and its version refusal will name the session rather than the build — `cargo build` rebuilds both (compound-026)",
                    util::contract(&host),
                    util::contract(&real),
                ));
            }
        }
    }

    match ask_wire_version(&sup) {
        Some(v) if v != WIRE_VERSION => problems.push(format!(
            "wsp's WIRE_VERSION is {WIRE_VERSION}, installed compound-sup speaks {v} — bump WIRE_VERSION in \
             src/place_compound.rs to match crates/supervisor/src/proto.rs (compound-026)"
        )),
        Some(_) => {}
        None => notes.push(format!(
            "{} does not answer `wire-version` — wsp cannot check its WIRE_VERSION ({WIRE_VERSION}) against it (compound-026)",
            util::contract(&sup),
        )),
    }
}

/// `compound-sup wire-version` → its VERSION, or `None` if the installed
/// binary predates that verb (an older build, not a broken one) or will not
/// answer at all. A current build prints the accepted range `OLDEST VERSION`;
/// VERSION is the last token. An older build prints a bare number, which is
/// the same token.
pub(crate) fn ask_wire_version(bin: &std::path::Path) -> Option<u32> {
    let out = Command::new(bin).arg("wire-version").stderr(Stdio::null()).output().ok()?;
    out.status.success().then(|| String::from_utf8_lossy(&out.stdout).split_whitespace().last()?.parse().ok()).flatten()
}

pub(crate) fn dial(socket: &Path, timeout: Duration) -> Result<UnixStream> {
    let deadline = Instant::now() + timeout;
    loop {
        match UnixStream::connect(socket) {
            Ok(s) => return Ok(s),
            Err(e) if Instant::now() >= deadline => {
                return Err(Refusal::Backend(format!("no answer from {}: {e}", socket.display())))
            }
            Err(_) => std::thread::sleep(Duration::from_millis(20)),
        }
    }
}

pub(crate) fn send(stream: &mut UnixStream, body: Value) -> std::io::Result<()> {
    let mut line = json!({ "ver": WIRE_VERSION }).as_object().unwrap().clone();
    line.extend(body.as_object().cloned().unwrap_or_default());
    let mut bytes = serde_json::to_vec(&line)?;
    bytes.push(b'\n');
    stream.write_all(&bytes)
}

/// One reply line, or `None` on EOF/timeout — a compound session that
/// answers nothing within the read timeout is read the same way a `tell`
/// with no watcher already reads: as `Unconfirmed`, not as a hang.
pub(crate) fn recv_line(stream: &mut UnixStream, timeout: Duration) -> Option<Value> {
    let _ = stream.set_read_timeout(Some(timeout));
    let mut reader = BufReader::new(stream.try_clone().ok()?);
    let mut line = String::new();
    match reader.read_line(&mut line) {
        Ok(0) | Err(_) => None,
        Ok(_) => serde_json::from_str(&line).ok(),
    }
}

/// Standard base64, matching `supervisor::proto`'s local encoder — the wire
/// carries `Input`'s bytes this way and there is no third-party dependency
/// worth pulling in for one field.
pub(crate) fn b64(data: &[u8]) -> String {
    const TABLE: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut out = String::with_capacity(data.len().div_ceil(3) * 4);
    for chunk in data.chunks(3) {
        let b = [chunk[0], *chunk.get(1).unwrap_or(&0), *chunk.get(2).unwrap_or(&0)];
        let n = ((b[0] as u32) << 16) | ((b[1] as u32) << 8) | b[2] as u32;
        out.push(TABLE[(n >> 18) as usize & 63] as char);
        out.push(TABLE[(n >> 12) as usize & 63] as char);
        out.push(if chunk.len() > 1 { TABLE[(n >> 6) as usize & 63] as char } else { '=' });
        out.push(if chunk.len() > 2 { TABLE[n as usize & 63] as char } else { '=' });
    }
    out
}

/// Type at a session and press Enter — [`Place::tell`], stripped of the
/// hook-state bookkeeping so a test can drive it against a bare listener.
///
/// **No `Attach`, and that is the whole of `compound-229`.** Since
/// `compound-117` the renderer answers `Input`, `Key`, `Mouse`, `Screen`
/// and `ReadSelection` to *every* connection and refuses `Attach` to a
/// second one — "busy: a client is already attached" — so a window holding
/// the pane made every `wsp tell` fail, focused or not, and `wsp` was the
/// one that could have been holding the lock out. The pre-117 comment here
/// said the wire refuses any other frame from an unclaimed connection; that
/// stopped being true at `compound-117` and this file kept the attach for
/// months after it. Only the frame stream and `Resize` still want the
/// attach, and this backend never reads frames or resizes anything.
pub(crate) fn type_and_submit(socket: &Path, text: &str) -> Result<()> {
    let mut stream = dial(socket, DIAL_TIMEOUT)?;
    send(&mut stream, json!({ "input": { "bytes": b64(text.as_bytes()) } }))
        .map_err(|e| Refusal::Backend(e.to_string()))?;
    press_enter(&mut stream)?;
    let _ = stream.shutdown(Shutdown::Both);
    Ok(())
}

/// `compound-sup deliver <socket> <id>` with the text on stdin, and its
/// one-line answer read: `delivered <id>` is [`Delivery::Acked`], `failed <id>
/// <why>` is [`Refusal::Failed`] (wsp-477). The id is wsp's, minted by the
/// caller and kept until an answer is read, so a wait that ran out is asked
/// again under it, not doubled.
///
/// The wait is bounded by `wait`: past it the call is left pending (an error
/// that is not [`Refusal::Failed`], so nothing is escalated and nothing is
/// stamped as typed) and the next tick asks again.
pub(crate) fn deliver_with(sup: &Path, session: &Path, id: &str, text: &str, wait: Duration, seat: &Seat) -> Result<Delivery> {
    let mut child = Command::new(sup)
        .arg("deliver")
        .arg(session)
        .arg(id)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|e| Refusal::Backend(format!("compound-sup deliver: {e}")))?;
    let pid = child.id();
    if let Some(mut stdin) = child.stdin.take() {
        let said = text.to_string();
        std::thread::spawn(move || {
            let _ = stdin.write_all(said.as_bytes());
        });
    }
    let (tx, rx) = mpsc::channel();
    std::thread::spawn(move || {
        let _ = tx.send(child.wait_with_output());
    });
    let out = match rx.recv_timeout(wait) {
        Ok(out) => out.map_err(|e| Refusal::Backend(format!("compound-sup deliver: {e}")))?,
        Err(_) => {
            let _ = Command::new("kill").arg(pid.to_string()).stderr(Stdio::null()).status();
            return Err(Refusal::Backend(format!(
                "{seat}: {} after {}s, asked again next tick under the same id",
                crate::place_compound::DELIVERY_PENDING,
                wait.as_secs()
            )));
        }
    };
    let (stdout, stderr) = (String::from_utf8_lossy(&out.stdout), String::from_utf8_lossy(&out.stderr));
    read_deliver(&stdout, &stderr, out.status.success(), id, seat)
}

/// The answer to a `deliver`, as a function of what it printed.
pub(crate) fn read_deliver(stdout: &str, stderr: &str, ok: bool, id: &str, seat: &Seat) -> Result<Delivery> {
    let line = stdout.lines().find(|l| !l.trim().is_empty()).unwrap_or("").trim();
    let mut words = line.splitn(3, ' ');
    match (words.next(), words.next()) {
        (Some("delivered"), Some(got)) if got == id => Ok(Delivery::Acked),
        (Some("failed"), Some(got)) if got == id => {
            Err(Refusal::Failed(format!("{seat}: {}", words.next().unwrap_or("no reason given").trim())))
        }
        // No session to ask: the seat is gone, which is its own answer.
        _ if stderr.starts_with("no session at") => Err(Refusal::NoSeat(seat.clone())),
        _ if ok => Err(Refusal::Backend(format!("compound-sup deliver answered `{line}`, which is neither delivered nor failed"))),
        _ => Err(Refusal::Backend(format!(
            "compound-sup deliver: {}",
            [line, stderr.trim()].iter().find(|s| !s.is_empty()).unwrap_or(&"no answer")
        ))),
    }
}

/// The submit key, as the render wire names it.
///
/// One definition, because a work order that is typed and not submitted and a
/// rescue press that presses a different key are the same defect wearing
/// different clothes, and this file had the sequence inline once already.
pub(crate) fn press_enter(stream: &mut UnixStream) -> Result<()> {
    send(
        stream,
        json!({ "key": { "event": {
            "action": "press",
            "key": "enter",
            "mods": { "shift": false, "ctrl": false, "alt": false, "cmd": false },
            "unshifted_codepoint": 0,
        }}}),
    )
    .map_err(|e| Refusal::Backend(e.to_string()))
}

/// The `compound-sup spawn` command for a seat, not yet run.
///
/// A function of its own so what it stands in is asserted without a
/// supervisor: `wsp-388`.
pub(crate) fn sup_command(sup: &Path, spawn: &Spawn<'_>) -> Command {
    let mut cmd = Command::new(sup);
    cmd.arg("spawn").args(["--label", spawn.seat]);
    // wsp-477: the kind the supervisor reads a composer for, which its
    // `deliver` needs — a seat spawned without one answers `failed`.
    if let Some(kind) = spawn.kind.as_deref().filter(|k| reads_kind(k)) {
        cmd.args(["--agent", kind]);
    }
    if !spawn.cwd.is_empty() {
        cmd.args(["--cwd", &spawn.cwd]);
    }
    // `wsp-432`: a supervisor a TEST starts belongs to the test process
    // (`compound-129`: it ends when this pid does, panic or abort
    // included) and stands in a run directory of its own, never the
    // person's `~/.compound/run`. Here and nowhere else, so a new test
    // cannot start one without both.
    #[cfg(any(test, feature = "test-support"))]
    crate::place_compound::test_sups::tie_to_this_test(&mut cmd);
    cmd.arg("--").arg(spawn.program).args(spawn.args);

    // Onto compound-sup's own environment, which its pty child inherits
    // in turn — the same "override, never replace" contract
    // `child_env` keeps, for the same reason: an empty
    // value is the only strip a spawned process's env accepts.
    for (k, v) in &spawn.env {
        match v.is_empty() {
            true => cmd.env_remove(k),
            false => cmd.env(k, v),
        };
    }
    for k in place::shed_keys() {
        cmd.env_remove(k);
    }

    // The supervisor stands nowhere. It used to keep the cwd of whoever ran
    // `wsp spawn`, so a seat opened from inside a check's tree had a
    // supervisor "in" that tree, and ending the check took it for the
    // check's own (`wsp-388`). The agent's directory is `--cwd`, above.
    if !spawn.cwd.is_empty() {
        cmd.current_dir("/");
    }
    cmd.stdin(Stdio::null()).stdout(Stdio::piped()).stderr(Stdio::null());
    cmd
}

/// Ends the pane of a seat whose agent has left: the supervisor, its shell and
/// the renderer, a few seconds on so the agent finishes its own exit. Detached
/// and in its own group, because the hook that calls it runs under the very
/// group it ends. The daemon's sweep (`ended_panes`) is the backstop for an ending
/// that never said `SessionEnd`.
pub(crate) fn close_pane_soon(sup: u32) {
    if sup == std::process::id() {
        return;
    }
    let script = format!("sleep {CLOSE_AFTER_SECS}; kill -TERM -- -{sup}; sleep 2; kill -KILL -- -{sup}");
    let _ = Command::new("sh")
        .args(["-c", &script])
        .process_group(0)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn();
}

/// Seconds an exited agent's pane stays before it is closed.
pub(crate) const CLOSE_AFTER_SECS: u32 = 2;

/// The forwards the seats on `machine` should have, as `(local socket, far
/// socket)`, read from the seat records. What the tunnel supervisor re-adds when
/// The newest modification time among a seat directory and the files in it,
/// in epoch seconds — [`crate::place::Place::quiet_since`] for both backends
/// that keep a directory per seat. The directory counts too: a file replaced
/// by rename moves the directory's time and not always the new file's.
pub fn last_written(dir: &Path) -> Option<i64> {
    let secs = |p: &Path| {
        let m = std::fs::metadata(p).ok()?.modified().ok()?;
        Some(m.duration_since(std::time::UNIX_EPOCH).ok()?.as_secs() as i64)
    };
    let files = std::fs::read_dir(dir).ok()?.flatten().filter_map(|e| secs(&e.path()));
    files.chain(secs(dir)).max()
}

/// The ledger of what stopped seats spent, one JSON line each, beside the seat
/// roots (`<state>/spent.jsonl`). `stop()` deletes the seat's directory and
/// its `burn.json` with it; this is the one copy that outlives it (wsp-264).
pub const SPENT_FILE: &str = "spent.jsonl";

/// Append the seat's burn record to the ledger before its directory goes.
/// Best effort and silent: a stop that cannot write a receipt still stops.
/// Same field names as `wsp burn --json`, so one reader serves both.
pub fn bank_burn(dir: &Path) {
    let rec = read_json(&dir.join(BURN_FILE));
    if rec.get("turns").and_then(|v| v.as_u64()).unwrap_or(0) == 0 {
        return;
    }
    let (Some(seat), Some(state)) = (
        dir.file_name().map(|n| n.to_string_lossy().to_string()),
        dir.parent().and_then(|r| r.parent()),
    ) else {
        return;
    };
    let n = |k: &str| rec.get(k).and_then(|v| v.as_u64()).unwrap_or(0);
    let line = serde_json::json!({
        "seat": seat,
        "ended": util::now_iso(),
        "session_id": str_of(&rec, "session_id"),
        "model": str_of(&rec, "model"),
        "turns": n("turns"),
        "input": n("input"),
        "output": n("output"),
        "cache_read": n("cache_read"),
        "cache_write": n("cache_write"),
        "cost": cost_of(&rec),
    });
    if let Ok(mut f) =
        fs::OpenOptions::new().create(true).append(true).open(state.join(SPENT_FILE))
    {
        let _ = writeln!(f, "{line}");
    }
}

/// Every seat's burn record under one root, seat id attached.
///
/// The reading half of [`tally_burn`] (`core-049`): the ranking is a question
/// about *where the tokens went*, and the answer lives one file per seat.
///
/// **The root is a parameter because two backends write these files and only
/// one of them grew a reader.** `tally_burn` is shared verbatim —
/// `place_compound` calls it at its own `heard` — so a compound seat tallies
/// exactly like a headless one, into a directory of its own
/// (`place_compound::SEATS`). This function living here as a method on
/// [`Supervisor`] therefore read `seats/`, and on a machine where every seat is
/// a compound one — which is the common case since `compound-112` — that
/// directory does not exist at all: `wsp burn` reported no burn for a fleet of
/// eight seats holding real records, and said so in the present tense
/// (`wsp-116`). Nothing about the *shape* was wrong; the reader was pointed at
/// one backend's store in a codebase whose seats are spread over two.
///
/// So the reader is a free function over a root, each backend reaches it
/// through its own `burn()`, and the report folds the two — the same
/// backend-then-compound fold `sync.rs`, `cmd_watch` and `cmd_checkout` already
/// make, and for the same reason: a source nobody folds is a source nobody is
/// looking at.
///
/// **A seat that tallied zero turns is skipped**, which is not the same thing
/// as a seat with no [`BURN_FILE`] and used to be documented as if it were. The
/// two come apart: a hook carrying a transcript whose lines carry no `usage` —
/// a kind that keeps none, or a format this does not read — writes a record
/// with `turns: 0` and real totals of zero, and it is dropped here. That is
/// right, and deliberately so: a seat at $0.00 is not a rank, and the report is
/// a ranking. A seat with *no* file is the case the doc used to name, and it is
/// dropped by the same line for a better reason — there is nothing there to
/// read.
pub fn burn_under(root: &Path) -> Vec<(String, Value)> {
    let Ok(entries) = std::fs::read_dir(root) else {
        return Vec::new();
    };
    let mut out = Vec::new();
    for e in entries.flatten() {
        let path = e.path();
        if !path.is_dir() {
            continue;
        }
        let rec = read_json(&path.join(BURN_FILE));
        if rec.get("turns").and_then(|v| v.as_u64()).unwrap_or(0) == 0 {
            continue;
        }
        let id = path
            .file_name()
            .map(|n| n.to_string_lossy().to_string())
            .unwrap_or_default();
        out.push((id, rec));
    }
    out.sort_by(|a, b| a.0.cmp(&b.0));
    out
}
/// The seat's environment, as the child will get it.
///
/// **An empty value is a removal here, and that is the port's own rule being
/// kept rather than a liberty.** `place::shed_env` empties rather than unsets
/// because a seat's environment on backend's wire is an override-only map and
/// there is no way to spell "unset" on it — the emptying is backend's compromise,
/// not the port's intention, and `place::seat_from_env` already reads an empty
/// value as an absence. A supervisor mints the seat and *then* forks, so it can
/// do the thing the wire could not.
///
/// The caller's own identity is shed whether or not the order mentioned it, for
/// the reason [`place::CHILD_MARKER`] carries: a session that inherits it writes
/// no transcript at all, and the only evidence is one truncated line in a pane
/// this backend does not have.
pub(crate) fn child_env(cmd: &mut Command, seat: &Seat, env: &BTreeMap<String, String>) {
    for (k, v) in env {
        match v.is_empty() {
            true => cmd.env_remove(k),
            false => cmd.env(k, v),
        };
    }
    for k in place::shed_keys() {
        cmd.env_remove(k);
    }
    // Last, and not from the order: the seat's own name is the one thing wsp
    // could not have put in `Order::env`, because it did not exist yet. This is
    // the direction `place.rs` reversed — the backend delivers the seat to
    // whatever runs in it, and says under what name.
    cmd.env(place::SEAT_ENV, seat.as_str());
}

/// `root`, everything descended from it, and everything in the process group
/// it leads — read in one `ps`.
///
/// **Both relations, because each misses a case the other catches.** A child
/// that called `setsid` has left the group and is still a descendant until its
/// parent dies; a process whose parent died is reparented to launchd and is
/// still in the group [`signal_group`] signals. An agent found at `ppid=1`
/// after its seat was closed is the second case, and it is the reason a caller
/// takes this *before* ending anything: afterwards the parent link is gone.
///
/// `ps` failing to run answers the root alone — not empty, so a caller asking
/// "is any of it still running?" still has the one pid it can certainly name.
pub fn tree_of(root: u32) -> Vec<u32> {
    let Some(rows) = process_rows() else { return vec![root] };
    let rows = &*rows;
    let mut tree = vec![root];
    let mut i = 0;
    while i < tree.len() {
        let parent = tree[i];
        for &(pid, ppid, _) in rows {
            if ppid == parent && !tree.contains(&pid) {
                tree.push(pid);
            }
        }
        i += 1;
    }
    for &(pid, _, pgid) in rows {
        if pgid == root && !tree.contains(&pid) {
            tree.push(pid);
        }
    }
    tree
}

thread_local! {
    /// The process table a [`one_ps`] scope holds: `None` outside one, and inside
    /// one `None` until the first [`tree_of`] has read it.
    static TABLE: std::cell::RefCell<Option<Option<std::rc::Rc<Vec<(u32, u32, u32)>>>>> = const { std::cell::RefCell::new(None) };
}

/// Every process as (pid, ppid, pgid), from one `ps` — or, inside [`one_ps`],
/// from the one that scope has already made. `None` when `ps` would not run.
fn process_rows() -> Option<std::rc::Rc<Vec<(u32, u32, u32)>>> {
    let held = TABLE.with(|t| t.borrow().clone());
    if let Some(Some(rows)) = held {
        return Some(rows);
    }
    let rows = Command::new("ps").args(["-A", "-o", "pid=,ppid=,pgid="]).output().ok().map(|out| {
        std::rc::Rc::new(
            String::from_utf8_lossy(&out.stdout)
                .lines()
                .filter_map(|l| {
                    let mut it = l.split_whitespace().map(|x| x.parse::<u32>().ok());
                    Some((it.next()??, it.next()??, it.next()??))
                })
                .collect::<Vec<(u32, u32, u32)>>(),
        )
    });
    if let (Some(_), Some(_)) = (&held, &rows) {
        TABLE.with(|t| *t.borrow_mut() = Some(rows.clone()));
    }
    rows
}

/// Run `f` with every [`tree_of`] in it answering from **one** `ps` taken at
/// its first call, rather than one per call (`wsp-480`). A scan that asks for
/// the tree of each seat of a fleet cost seventy `ps` runs, 12 s of a thermally
/// limited machine, for what one table answers. Nothing is kept past `f`: a
/// caller that signals and looks again takes a scope for each look.
pub fn one_ps<T>(f: impl FnOnce() -> T) -> T {
    let outer = TABLE.with(|t| t.borrow_mut().replace(None));
    let out = f();
    TABLE.with(|t| *t.borrow_mut() = outer);
    out
}

/// Signal a whole process group, by the id of the group's leader.
///
/// The group rather than the pid, because an agent's children are its work: a
/// `Bash` tool call is a shell under the agent, and a `stop` that left it running
/// would be the sort of half-ending that leaves a build writing into a tree wsp
/// has told somebody else is free.
pub fn signal_group(group: u32, sig: &str) {
    // Never this process's own group: a record naming our own pid (a test's
    // stand-in for a live supervisor) would otherwise end us when we are a group
    // leader, as every test is under `cargo nextest` (wsp-440).
    if group == std::process::id() {
        return;
    }
    let _ = Command::new("kill")
        .arg(format!("-{sig}"))
        .arg(format!("-{group}"))
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status();
}
/// Add this hook's slice of the transcript to the seat's running total.
///
/// Every request an agent makes re-reads its whole context, so **where the
/// tokens are is the question `core-049` was filed to answer** — and Claude
/// Code already writes the answer: every assistant message in the transcript
/// carries a `usage` object with the input, output and cache counts of the
/// request that produced it. Nothing else in wsp sees a transcript — they are
/// session-private — so the hook is where this is read, one append at a time.
///
/// The tally is incremental and survives being wrong about nothing: a byte
/// offset into the file says how far the last look got, only new *whole* lines
/// are parsed and only whole lines advance the offset (a Stop fires every turn,
/// so re-reading a whole night's transcript each time would be quadratic in
/// exactly the sessions this exists to measure), a shorter file than remembered
/// means it was rotated or truncated and the count restarts rather than
/// double-counts, and a different `session_id` means the seat was cleared —
/// `/clear` ends an accounting as surely as it ends a context. A transcript
/// with no `usage` lines (a kind that keeps none, or a format wsp does not
/// read) tallies zero and costs one pass.
///
/// Two things it counts *once* that the first draft counted wrong, both of them
/// silent: the fragment at the end of a read, and the repeated `usage` object
/// Claude Code writes per content block. The argument for each is at the line
/// that does it.
///
/// Written for [`heard`], which every hook reaches; best-effort like everything
/// else on that path.
pub fn tally_burn(dir: &PathBuf, payload: &Value) {
    let path = str_of(payload, "transcript_path");
    if path.is_empty() {
        return;
    }
    let sid = str_of(payload, "session_id");
    let was = read_json(&dir.join(BURN_FILE));
    // Same session continues the running total; anything else starts one. An
    // empty stored id is a seat that has not been tallied yet, which is also a
    // start.
    let same = !sid.is_empty() && str_of(&was, "session_id") == sid;
    let mut offset = if same {
        was.get("offset").and_then(|v| v.as_u64()).unwrap_or(0)
    } else {
        0
    };
    let mut input = if same { u_at(&was, "input") } else { 0 };
    let mut output = if same { u_at(&was, "output") } else { 0 };
    let mut cache_read = if same { u_at(&was, "cache_read") } else { 0 };
    let mut cache_write = if same { u_at(&was, "cache_write") } else { 0 };
    let mut turns = if same { u_at(&was, "turns") } else { 0 };
    let mut model = if same { str_of(&was, "model") } else { String::new() };
    // Not `u_at`: a record from before the tally priced anything has totals and
    // no cost, and starting from zero would report a whole night as costing
    // whatever the next few turns did.
    let mut cost = if same { cost_of(&was) } else { 0 };
    // The last request counted, carried between hooks so the dedupe below
    // survives a slice boundary landing in the middle of one.
    let mut last = if same { str_of(&was, "last_request") } else { String::new() };

    let Ok(file) = fs::File::open(&path) else {
        return;
    };
    let len = file.metadata().map(|m| m.len()).unwrap_or(0);
    if len < offset {
        // Rotated, swept or truncated: whatever happened, counting from a hole
        // would count somebody twice.
        offset = 0;
        (input, output, cache_read, cache_write, turns, cost) = (0, 0, 0, 0, 0, 0);
        last.clear();
    }
    use std::io::{Read as _, Seek as _, SeekFrom};
    let mut reader = std::io::BufReader::new(file);
    if reader.seek(SeekFrom::Start(offset)).is_err() {
        return;
    }
    let mut fresh = Vec::new();
    if reader.read_to_end(&mut fresh).is_err() {
        return;
    }
    let text = match String::from_utf8(fresh) {
        Ok(t) => t,
        Err(_) => return,
    };
    // Only whole lines are counted, and only whole lines are consumed. A hook
    // fires on the writer's clock, not on the writer's line endings, so a read
    // can land inside a record that is still being written — and an offset
    // advanced past the fragment would leave the next read starting inside a
    // record it can no longer parse, dropping it for good. Silent undercount in
    // an instrument whose whole job is counting.
    let whole = text.rfind('\n').map(|i| i + 1).unwrap_or(0);
    for line in text[..whole].lines() {
        let Ok(line) = serde_json::from_str::<Value>(line) else {
            continue;
        };
        let Some(message) = line.get("message") else {
            continue;
        };
        let Some(usage) = message.get("usage") else {
            continue;
        };
        // One request, one bill. Claude Code writes a transcript line per
        // *content block* and repeats the request's whole `usage` object on
        // each of them, so an assistant turn that spoke and then called a tool
        // is three identical lines. Measured on this store on 2026-08-26: 49
        // usage lines over 30 requests, which a naive sum reports as 1.6x the
        // real bill. The blocks of one request are written together, so
        // remembering the last id counted is enough to tell a repeat from a
        // new request.
        let id = match str_of(&line, "requestId") {
            // `message.id` is the same key one level in, and the fallback for a
            // transcript that carries no request id of its own.
            s if s.is_empty() => str_of(message, "id"),
            s => s,
        };
        if !id.is_empty() && id == last {
            continue;
        }
        if !id.is_empty() {
            last = id;
        }
        let n = |k: &str| usage.get(k).and_then(|v| v.as_u64()).unwrap_or(0);
        let (i, o) = (n("input_tokens"), n("output_tokens"));
        let (cr, cw) = (n("cache_read_input_tokens"), n("cache_creation_input_tokens"));
        input += i;
        output += o;
        cache_read += cr;
        cache_write += cw;
        turns += 1;
        // Priced against the model *this request* ran on, not against the one
        // the session opened with: an agent that types `/model` mid-session is
        // billed at both tiers and neither total is a lie about the other. The
        // stored `model` is therefore what the seat is on now, and the cost is
        // not derivable from it — see [`cost`].
        //
        // A request that carried no tokens did not run on anything, so it has
        // no tier to report, whatever it called itself: Claude Code writes
        // zero-usage `<synthetic>` records (a cancelled turn, a refusal it composed) and
        // one at the tail of a transcript would otherwise rename a seat that
        // spent every dollar on a real tier.
        let ran_on = str_of(message, "model");
        cost += self::cost(&ran_on, i, o, cr, cw);
        if !ran_on.is_empty() && i + o + cr + cw > 0 {
            model = ran_on;
        }
    }

    let _ = write_atomic(
        &dir.join(BURN_FILE),
        &json!({
            "session_id": sid,
            "transcript": path,
            "offset": offset + whole as u64,
            "model": model,
            "last_request": last,
            "cost": cost,
            "input": input,
            "output": output,
            "cache_read": cache_read,
            "cache_write": cache_write,
            "turns": turns,
            "updated": util::now_iso(),
        })
        .to_string(),
    );
}

/// A counter out of a burn record, absent meaning zero — the shape a first
/// tally reads back as.
pub(crate) fn u_at(v: &Value, key: &str) -> u64 {
    v.get(key).and_then(|x| x.as_u64()).unwrap_or(0)
}
