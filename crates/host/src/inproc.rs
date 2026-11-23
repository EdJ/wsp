//! The in-process host: every OS operation a seat needs, run in this process
//! (`docs/agent-manager.md` section 6.2, M0 and M1).
//!
//! [`InProcess`]'s methods are the bodies of `wsp-wire`'s `host::Host` trait,
//! which stays in the wire as a thin adapter calling each of them by name
//! (wsp-020). The free functions below them are what those methods are built
//! from: the `ps`, `kill` and `lsof` of seats and tree work, the screen read
//! through `compound-render`, delivery through `compound-sup`, the hook files
//! and burn tally of a seat's directory, and the transcript readers. The guard
//! in `tests/host_seam.rs` allows these probes here and nowhere else, and fails
//! a caller outside this crate that names one of the OS-touching free
//! functions (`signal_group`, `tree_of`, `tally_burn`, ...) past the trait.
//!
//! What this module needed of the wire came down with it: the port's words
//! ([`crate::port`]), the shed list ([`crate::shed`]), the prices
//! ([`crate::price`]), and below, `compound-sup`'s [`WIRE_VERSION`],
//! [`DELIVERY_PENDING`], [`reads_kind`], [`read_json`] and [`str_of`]. The wire
//! re-exports each where it used to be defined.

#![allow(dead_code)]

use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::io::{BufRead, BufReader, Write};
use std::net::Shutdown;
use std::os::unix::net::UnixStream;
use std::path::{Path, PathBuf};
use std::os::unix::fs::OpenOptionsExt;
use std::process::Stdio;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::mpsc;
use std::time::{Duration, Instant};
use wsp_base::calls::Command;

use serde_json::{json, Value};

use wsp_base::base::seat::SEAT_ENV;
use wsp_base::util::{self, Clock};

use crate::port::{Delivery, Refusal, Result, Seat, State};
use crate::price::{cost, cost_of};
use crate::shed::shed_keys;

/// What the agent's hooks last said about it, one record, rewritten whole.
pub const SAID_FILE: &str = "said.json";
/// Every hook a compound seat has been heard from, one JSON line each.
pub const HOOKS_FILE: &str = "hooks.jsonl";
/// What the session has cost so far, tallied from its transcript.
pub const BURN_FILE: &str = "burn.json";

/// How long `spawn_session` waits for `compound-sup`'s handshake.
const HANDSHAKE_TIMEOUT: Duration = Duration::from_secs(5);
/// How long a dial to a session's socket waits for it to accept.
pub const DIAL_TIMEOUT: Duration = Duration::from_secs(2);

/// `seat.spawn` for a pty seat: what runs in it and where, already decided.
pub struct Spawn<'a> {
    /// The idempotency key `seat.spawn` carries over the wire (3.3): the row
    /// and its attempt. `InProcess` ignores it; `host::Tcp` sends it, built once
    /// per spawn so a retry after a lost reply reaches the manager under it.
    pub key: wsp_host_api::Key,
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

/// What watches a seat's `tail -f` feed: `$1` the agent's pid, `$2` the feed's
/// (the group's leader), `$3` the seat's directory. It ends the feed when the
/// agent is gone or the directory is, which is the only thing that can tell
/// `tail` it has no reader — it learns of one only when it next writes.
const FEED_WATCH: &str = r#"while kill -0 "$1" 2>/dev/null && [ -d "$3" ]; do sleep 1; done; kill "$2" 2>/dev/null; exit 0"#;

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


/// The host this process is: today's code, run here.
pub struct InProcess {
    /// A `compound-sup` named by a test; `None` finds the installed one.
    sup: Option<PathBuf>,
}

impl InProcess {
    pub const fn new() -> InProcess {
        InProcess { sup: None }
    }

    /// One whose `compound-sup` is this file — a test's script.
    pub fn with_sup(sup: &Path) -> InProcess {
        InProcess { sup: Some(sup.to_path_buf()) }
    }

    pub fn sup(&self) -> Option<PathBuf> {
        self.sup.clone().or_else(compound_sup_binary)
    }
}

impl Default for InProcess {
    fn default() -> InProcess {
        InProcess::new()
    }
}

impl InProcess {
    pub fn spawn_session(&self, spawn: &Spawn<'_>) -> Result<Session> {
        let Some(sup) = self.sup() else {
            return Err(Refusal::Backend("compound-sup not found — set $COMPOUND_SUP or put it on PATH".into()));
        };
        let mut cmd = sup_command(&sup, spawn);
        let mut child = cmd.spawn().map_err(|e| Refusal::Backend(format!("{} did not start: {e}", sup.display())))?;
        let pid = child.id();
        #[cfg(any(test, feature = "test-support"))]
        crate::test_sups::note(pid, &Seat::new(spawn.seat));
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

    pub fn spawn_headless(&self, spawn: &Headless<'_>) -> Result<Forked> {
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
            Ok(child) => {
                // The feed ends with its owner. `stop` signals the group, but a seat
                // whose agent exits on its own, or whose directory goes without a
                // `stop` (a sandbox removed, a test killed), would otherwise leave this
                // `tail -f` idle for good — 20 of them were 15 days old (wsp-664). In
                // the agent's group, so a `stop` ends it too.
                let dir = spawn.prompts.parent().unwrap_or(Path::new("."));
                let _ = Command::new("sh")
                    .args(["-c", FEED_WATCH, "sh"])
                    .arg(child.id().to_string())
                    .arg(group.to_string())
                    .arg(dir)
                    .stdin(Stdio::null())
                    .stdout(Stdio::null())
                    .stderr(Stdio::null())
                    .process_group(group as i32)
                    .spawn();
                Ok(Forked { pid: child.id(), group })
            }
            Err(e) => {
                // The feed is not left running against a seat with no agent in
                // it: a `tail -f` nobody is reading is a leak that would outlive
                // every failed spawn.
                signal_group(group, "KILL");
                Err(Refusal::Backend(format!("{} did not start: {e}", spawn.program)))
            }
        }
    }

    pub fn stop(&self, stop: Stop<'_>) {
        signal_group(stop.group, "TERM");
        let Some(pid) = stop.watch else { return };
        let deadline = stop.clock.now() + stop.grace;
        while self.is_alive(pid) {
            if stop.clock.now() >= deadline {
                signal_group(stop.group, "KILL");
                break;
            }
            stop.clock.rest(stop.poll);
        }
    }

    pub fn stop_soon(&self, group: u32) {
        close_pane_soon(group)
    }

    pub fn clear_socket(&self, socket: &Path, sidecars: &[&str]) {
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

    pub fn unlink(&self, path: &Path) {
        let _ = fs::remove_file(path);
    }

    pub fn alive(&self, pids: &[u32]) -> BTreeSet<u32> {
        alive(pids)
    }

    /// Whether this one pid is running: [`InProcess::alive`] for one, as the
    /// trait's default answers it.
    pub fn is_alive(&self, pid: u32) -> bool {
        self.alive(&[pid]).contains(&pid)
    }

    pub fn started(&self, pid: u32) -> std::result::Result<Option<String>, ()> {
        let out = Command::new("ps").args(["-o", "lstart=,stat=", "-p", &pid.to_string()]).output().map_err(|_| ())?;
        wsp_base::base::procs::ps_says(out.status.code(), &out.stdout)
    }

    pub fn shells(&self) -> Option<String> {
        let out = Command::new("ps").args(["-A", "-o", "pid=,ppid=,etime=,time=,command="]).output().ok()?;
        Some(String::from_utf8_lossy(&out.stdout).into_owned())
    }

    pub fn seat_memory(&self) -> Option<String> {
        succeeded(Command::new("ps").args(["-A", "-E", "-o", "pid=,ppid=,rss=,command="]))
    }

    pub fn disk_free(&self, path: &Path) -> Option<String> {
        succeeded(Command::new("df").args(["-Pk"]).arg(path))
    }

    /// `df -Pk` of every path in one ask: a header, then a row per path.
    pub fn disks_free(&self, paths: &[&Path]) -> Option<String> {
        if paths.is_empty() {
            return None;
        }
        // A path that does not exist fails its row and the status, not the others.
        let out = Command::new("df").arg("-Pk").args(paths).stderr(Stdio::null()).output().ok()?;
        Some(String::from_utf8_lossy(&out.stdout).into_owned())
    }

    /// `sysctl` of several names in one ask, as `name: value` lines. A name
    /// this host lacks is missing from the answer, not a failure of the rest.
    pub fn sysctls(&self, names: &[&str]) -> Option<String> {
        let out = Command::new("sysctl").args(names).stderr(Stdio::null()).output().ok()?;
        Some(String::from_utf8_lossy(&out.stdout).into_owned())
    }

    /// This host's tailnet addresses, as `tailscale ip` gives them, one per
    /// line; `None` when no tailscale answers.
    pub fn tailnet_ips(&self) -> Option<String> {
        const APP: &str = "/Applications/Tailscale.app/Contents/MacOS/Tailscale";
        ["tailscale", APP]
            .into_iter()
            .find_map(|bin| succeeded(Command::new(bin).arg("ip").stderr(Stdio::null())))
    }

    pub fn disk_use(&self, path: &Path) -> Option<String> {
        let out = Command::new("du").args(["-sk"]).arg(path).output().ok()?;
        Some(String::from_utf8_lossy(&out.stdout).into_owned())
    }

    pub fn load(&self) -> Option<String> {
        if let Ok(s) = fs::read_to_string("/proc/loadavg") {
            return Some(s);
        }
        let out = Command::new("sysctl").args(["-n", "vm.loadavg"]).stderr(Stdio::null()).output().ok()?;
        Some(String::from_utf8_lossy(&out.stdout).into_owned())
    }

    pub fn thermal(&self) -> Option<String> {
        succeeded(Command::new("pmset").args(["-g", "therm"]).stderr(Stdio::null()))
    }

    pub fn memory_pressure(&self) -> Option<String> {
        succeeded(Command::new("memory_pressure").stderr(Stdio::null()))
    }

    pub fn swap(&self) -> Option<String> {
        succeeded(Command::new("sysctl").args(["-n", "vm.swapusage"]).stderr(Stdio::null()))
    }

    /// `/proc/meminfo`: Linux's memory and swap figures; `None` where there is
    /// no `/proc` (macOS answers through `sysctl` and `vm_stat`).
    pub fn meminfo(&self) -> Option<String> {
        fs::read_to_string("/proc/meminfo").ok()
    }

    /// `/proc/vmstat`: Linux's paging counters (`pswpout`); `None` off Linux.
    pub fn vmstat(&self) -> Option<String> {
        fs::read_to_string("/proc/vmstat").ok()
    }

    pub fn pageouts(&self) -> Option<String> {
        succeeded(Command::new("vm_stat").stderr(Stdio::null()))
    }

    pub fn idle(&self) -> Option<String> {
        succeeded(Command::new("ioreg").args(["-c", "IOHIDSystem", "-d", "4"]).stderr(Stdio::null()))
    }

    pub fn tree_of(&self, root: u32) -> Vec<u32> {
        tree_of(root)
    }

    pub fn table(&self, columns: &str, env: bool) -> Option<String> {
        let mut cmd = Command::new("ps");
        cmd.arg("-A");
        if env {
            cmd.arg("-E");
        }
        let out = cmd.args(["-o", columns]).stdin(Stdio::null()).stderr(Stdio::null()).output().ok()?;
        let table = out.status.success().then(|| String::from_utf8_lossy(&out.stdout).into_owned())?;
        if cfg!(target_os = "linux") {
            return Some(comm_as_run(&table, columns, |pid| fs::read(format!("/proc/{pid}/cmdline")).ok()));
        }
        Some(table)
    }

    /// This process's own children as a reloaded daemon adopts them (wsp-667,
    /// `wsp_base::calls::children_of`): `ps -axo pid=,ppid=,command=`, and the
    /// pid of that `ps`, which is a child of this process too and on its own
    /// list. Spawned with `std`'s `Command`, not the counting wrapper, so the
    /// `ps` is held by nothing here while it runs. `None` when it would not run.
    pub fn own_children(&self) -> Option<(u32, String)> {
        let ps = std::process::Command::new("ps")
            .args(["-axo", "pid=,ppid=,command="])
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()
            .ok()?;
        let itself = ps.id();
        let out = ps.wait_with_output().ok()?;
        Some((itself, String::from_utf8_lossy(&out.stdout).into_owned()))
    }

    /// The table the daemon's reaper names defunct strays from (wsp-664/667,
    /// `wsp_base::calls::reap_defunct_with`): `ps -axo pid=,ppid=,stat=,comm=`,
    /// whatever its status. `None` when `ps` would not run.
    pub fn own_defunct(&self) -> Option<String> {
        let out = std::process::Command::new("ps").args(["-axo", "pid=,ppid=,stat=,comm="]).output().ok()?;
        Some(String::from_utf8_lossy(&out.stdout).into_owned())
    }

    /// Whether `pid` runs and this process may signal it: kill(2) with signal 0,
    /// what `kill -0 <pid>` answers, asked without running `kill` (wsp-076: `wip`
    /// through the API runs no child). Pid 0 and pids past `i32` are no process —
    /// kill(2) would read 0 as this process's group and a negative as another's.
    pub fn pid_runs(&self, pid: u32) -> bool {
        pid_runs(pid)
    }

    pub fn rows_of(&self, pids: &[u32], columns: &str) -> Option<String> {
        if pids.is_empty() {
            return Some(String::new());
        }
        let list = pids.iter().map(u32::to_string).collect::<Vec<_>>().join(",");
        // `ps -p` fails when any pid has gone and still lists the rest.
        let out = Command::new("ps").args(["-o", columns, "-p", &list]).stdin(Stdio::null()).stderr(Stdio::null()).output().ok()?;
        Some(String::from_utf8_lossy(&out.stdout).into_owned())
    }

    pub fn signal(&self, pids: &[u32], sig: &str) -> Option<bool> {
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

    pub fn signal_group(&self, group: u32, sig: &str) {
        signal_group(group, sig)
    }

    pub fn answers(&self, socket: &Path) -> bool {
        UnixStream::connect(socket).is_ok()
    }

    pub fn read_screen(&self, render: &Path) -> Result<String> {
        let bin = compound_render_binary().ok_or_else(|| {
            Refusal::Backend("no renderer (compound-render or compound-reader) found beside compound-sup or on PATH — set $COMPOUND_RENDER".into())
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

    pub fn screen_state(&self, render: &Path, kind: &str) -> Option<State> {
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

    pub fn osc_title(&self, socket: &Path) -> Option<String> {
        let sup = self.sup()?;
        let out = Command::new(&sup).args(["vitals"]).arg(socket).output().ok()?;
        if !out.status.success() {
            return None;
        }
        String::from_utf8_lossy(&out.stdout).lines().find_map(|l| l.strip_prefix("osc_title ").map(str::to_string))
    }

    pub fn can_deliver(&self) -> bool {
        self.sup().is_some()
    }

    pub fn deliver(&self, session: &Path, id: &str, text: &str, wait: Duration, seat: &Seat) -> Result<Delivery> {
        let sup = self
            .sup()
            .ok_or_else(|| Refusal::Backend("compound-sup not found — set $COMPOUND_SUP or put it on PATH".into()))?;
        deliver_with(&sup, session, id, text, wait, seat)
    }

    pub fn type_in(&self, render: &Path, text: &str) -> Result<()> {
        type_and_submit(render, text)
    }

    pub fn press_enter(&self, render: &Path) -> Result<()> {
        let mut stream = dial(render, DIAL_TIMEOUT)?;
        press_enter(&mut stream)?;
        let _ = stream.shutdown(Shutdown::Both);
        Ok(())
    }

    pub fn append_line(&self, file: &Path, line: &str) -> Result<()> {
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

    pub fn said(&self, dir: &Path) -> Value {
        read_json(&dir.join(SAID_FILE))
    }

    pub fn say(&self, dir: &Path, said: &Value) {
        let _ = write_atomic_into(&dir.join(SAID_FILE), &said.to_string());
    }

    pub fn log_hook(&self, dir: &Path, line: &Value) {
        // Best-effort, like everything else a hook touches — a write that
        // fails here must not be the write that fails the hook.
        if let Ok(mut f) = fs::OpenOptions::new().create(true).append(true).open(dir.join(HOOKS_FILE)) {
            let _ = writeln!(f, "{line}");
        }
    }

    pub fn quiet_since(&self, dir: &Path) -> Option<i64> {
        last_written(dir)
    }

    pub fn tally(&self, dir: &Path, payload: &Value) {
        tally_burn(&dir.to_path_buf(), payload)
    }

    pub fn bank(&self, dir: &Path) {
        bank_burn(dir)
    }

    pub fn burn_under(&self, root: &Path) -> Vec<(String, Value)> {
        burn_under(root)
    }

    pub fn transcript_of(&self, session: &str, cwd: &str) -> Option<PathBuf> {
        claude_transcript(session, cwd)
    }

    pub fn transcripts_of_tree(&self, tree: &str) -> Vec<PathBuf> {
        claude_transcripts_of_tree(tree)
    }

    pub fn pi_session_of(&self, session: &str, cwd: &str) -> Option<PathBuf> {
        pi_session_file(session, cwd)
    }

    pub fn read_transcript(&self, file: &Path) -> Option<Box<dyn BufRead>> {
        Some(Box::new(BufReader::new(fs::File::open(file).ok()?)))
    }

    pub fn transcript_bytes(&self, file: &Path) -> Option<Vec<u8>> {
        fs::read(file).ok()
    }

    pub fn transcript_tail(&self, file: &Path, bytes: u64) -> Option<String> {
        tail_of(file, bytes)
    }

    /// One session as opencode publishes it, redacted of everything but the
    /// numbers.
    ///
    /// The id is checked before it reaches a command line, for the reason
    /// [`claude_transcript`] checks it before it reaches a path: it arrives from the
    /// backend, which got it from the agent, and `spawn` is a verb agents drive. opencode
    /// spells an id `ses_` and twenty-something alphanumerics; anything with a
    /// space, a slash or a dash in it is not one.
    ///
    /// Taken through [`captured`] and not through a pipe, which is the whole of
    /// `core-039`: this is the longest thing wsp asks any runtime for, so it is the
    /// one that overran a pipe buffer and came back a truncated document under a
    /// successful exit status. The argument is there.
    pub fn opencode_export(&self, session: &str) -> Option<Vec<u8>> {
        if !opencode_id(session) {
            return None;
        }
        captured(opencode_bin(), &["export", "--sanitize", session]).ok()
    }

    pub fn opencode_newest(&self, session: &str) -> Option<Vec<Value>> {
        if !opencode_id(session) {
            return None;
        }
        opencode_rows(session)
    }

    pub fn wire_health(&self, problems: &mut Vec<String>, notes: &mut Vec<String>) {
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
/// A probe's stdout, when it ran and exited 0.
fn succeeded(cmd: &mut Command) -> Option<String> {
    let out = cmd.output().ok().filter(|o| o.status.success())?;
    Some(String::from_utf8_lossy(&out.stdout).into_owned())
}

/// kill(2) with signal 0 to `pid`, for [`InProcess::pid_runs`]; moved from
/// `wsp_base::util` (wsp-111), whose C declaration of `kill` was below the seam.
/// Unlike [`alive`], it reports a zombie as running: kill(2) answers for any
/// pid still in the process table.
pub(crate) fn pid_runs(pid: u32) -> bool {
    extern "C" {
        fn kill(pid: i32, sig: std::ffi::c_int) -> std::ffi::c_int;
    }
    match i32::try_from(pid) {
        // SAFETY: signal 0 delivers nothing; it only checks the pid.
        Ok(p) if p > 0 => unsafe { kill(p, 0) == 0 },
        _ => false,
    }
}

/// Which of these pids are running, in one ask.
///
/// One `ps` rather than one `kill -0` each, because `Place::census` would
/// otherwise fork once per seat. An empty list forks nothing.
///
/// **A zombie is not alive**, and that arm is not hypothetical. `wsp spawn`
/// starts the agent and then polls its readiness from the same process, so an
/// agent that dies in its first second is a child nobody has reaped and sits in
/// the process table looking exactly like a running one. Reporting it as alive
/// is the same class of defect as robustness-041 with the sign flipped: an agent
/// declared healthy while its corpse cools.
pub(crate) fn alive(pids: &[u32]) -> std::collections::BTreeSet<u32> {
    if pids.is_empty() {
        return std::collections::BTreeSet::new();
    }
    let list = pids.iter().map(|p| p.to_string()).collect::<Vec<_>>().join(",");
    let out = match Command::new("ps").args(["-o", "pid=,state=", "-p", &list]).output() {
        Ok(o) => o,
        // Not an empty set: `ps` failing to run is not evidence that every agent
        // on this machine has stopped, and a caller that reaped on the strength
        // of it would end every claim at once. The rule is `sync.rs:41`'s and is
        // older than this file.
        Err(_) => return pids.iter().copied().collect(),
    };
    String::from_utf8_lossy(&out.stdout)
        .lines()
        .filter_map(|l| {
            let mut it = l.split_whitespace();
            let pid: u32 = it.next()?.parse().ok()?;
            match it.next().unwrap_or("").starts_with('Z') {
                true => None,
                false => Some(pid),
            }
        })
        .collect()
}

pub fn compound_sup_binary() -> Option<PathBuf> {
    let exe_dir = wsp_base::util::self_exe().ok().and_then(|e| e.parent().map(PathBuf::from));
    find_sup(
        std::env::var_os("COMPOUND_SUP"),
        std::env::var_os("PATH"),
        std::env::var_os("HOME").map(PathBuf::from),
        exe_dir,
    )
}

pub fn find_sup(
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

#[cfg(any(test, feature = "test-support"))]
thread_local! {
    /// A stand-in `compound-render` for this thread's calls to
    /// [`compound_render_binary`].
    pub static RENDER: std::cell::RefCell<Option<PathBuf>> = const { std::cell::RefCell::new(None) };
}

/// Where the renderer lives: `$COMPOUND_RENDER`, else beside whichever
/// `compound-sup` this process is already using, else `PATH`.
///
/// Beside the supervisor comes first (`compound-136`): the two binaries are
/// built together into one `cargo build --target-dir` (`compound-sup`'s own
/// `renderer_binary_path` takes the identical shortcut, one process over), so
/// the renderer next to the supervisor is the one that speaks its wire. A
/// stray one earlier on `PATH` is the fallback, not the answer.
pub fn compound_render_binary() -> Option<PathBuf> {
    // A test's stand-in, per thread so no other test meets it.
    #[cfg(any(test, feature = "test-support"))]
    if let Some(p) = RENDER.with(|r| r.borrow().clone()) {
        return Some(p);
    }
    if let Some(p) = std::env::var_os("COMPOUND_RENDER") {
        let p = PathBuf::from(p);
        return p.is_file().then_some(p);
    }
    find_render(std::env::var_os("PATH"), compound_sup_binary().and_then(|s| s.parent().map(PathBuf::from)))
}

/// What the renderer binary is called, first guess first. The supervisor
/// starts `compound-render` on macOS and `compound-reader` on Linux, where
/// compound split its renderer from its reader and kept only the latter name;
/// both answer the same `screen` and `state` verbs. Without the second name a
/// Linux machine reads every seat's screen as "cannot read", and a seat whose
/// hooks have aged past `VOUCH_SECS` reads `unknown`.
pub const RENDER_NAMES: [&str; 2] =
    if cfg!(target_os = "macos") { ["compound-render", "compound-reader"] } else { ["compound-reader", "compound-render"] };

pub fn find_render(path: Option<std::ffi::OsString>, beside_sup: Option<PathBuf>) -> Option<PathBuf> {
    let beside = beside_sup.into_iter();
    let dirs: Vec<PathBuf> = beside.chain(path.iter().flat_map(std::env::split_paths)).collect();
    dirs.iter().find_map(|d| RENDER_NAMES.iter().map(|name| d.join(name)).find(|p| p.is_file()))
}

/// `compound-sup`'s four-line handshake since `compound-101`'s split — `sup
/// <pid>`, `pid <pid>`, `socket <path>`, `render <path>` — read with a
/// deadline off a background thread so a supervisor that never prints one (a
/// bad build, a hung `zig` toolchain, anything short of the ordinary path)
/// cannot hang `Place::start` rather than refuse it.
///
/// Only `socket` and `render` are parsed; `sup`'s pid is `Command::spawn`'s
/// own answer already, and `pid` is read from the record this same call
/// writes. Reads up to four lines rather than stopping the instant `render`
/// is seen, so an even older, pre-split `compound-sup` — three lines, no
/// `render` — still hands back its socket rather than timing out waiting for
/// a fourth line that binary will never print.
pub fn read_handshake(stdout: std::process::ChildStdout, timeout: Duration) -> Option<(PathBuf, Option<PathBuf>)> {
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
/// `wire_health`, given the binary rather than finding it — split out so a
/// test can point this at a fake script instead of racing every other test in
/// this process over `$COMPOUND_SUP` (`wsp-env-breaks-cargo-test`'s lesson,
/// one door over: a global is a global whether it names a store or a binary).
pub fn wire_health_of(sup: &std::path::Path, problems: &mut Vec<String>, notes: &mut Vec<String>) {
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

    // A current build answers the range it accepts (`OLDEST VERSION`), and
    // a WIRE_VERSION inside it is answered, not refused: wsp sends no
    // fallback, so staying at an older accepted version is what keeps it
    // speaking to an older compound-sup too. Only a constant outside the
    // range is the refusal compound-026 was filed for.
    match ask_wire_range(&sup) {
        Some(Ok((lo, hi))) if !(lo..=hi).contains(&WIRE_VERSION) => {
            let range = if lo == hi { hi.to_string() } else { format!("{lo}..={hi}") };
            problems.push(if WIRE_VERSION > hi {
                format!(
                    "wsp's WIRE_VERSION is {WIRE_VERSION}, installed compound-sup accepts {range} — compound-sup is \
                     behind: rebuild and reinstall it from a compound commit that speaks {WIRE_VERSION} (compound-026)"
                )
            } else {
                format!(
                    "wsp's WIRE_VERSION is {WIRE_VERSION}, installed compound-sup accepts {range} — bump WIRE_VERSION in \
                     src/place_compound.rs to match crates/supervisor/src/proto.rs (compound-026)"
                )
            })
        }
        Some(Ok(_)) => {}
        Some(Err(said)) => problems.push(format!(
            "{} answered `wire-version` with {said:?}, which is not a version range — wsp cannot check its WIRE_VERSION ({WIRE_VERSION}) against it (compound-026)",
            util::contract(&sup),
        )),
        None => notes.push(format!(
            "{} does not answer `wire-version` — wsp cannot check its WIRE_VERSION ({WIRE_VERSION}) against it (compound-026)",
            util::contract(&sup),
        )),
    }
}

/// `compound-sup wire-version` → the range it accepts (`Ok`), what it said
/// if that is not a range (`Err`), or `None` if the installed binary predates
/// that verb (an older build, not a broken one) or will not answer at all. A
/// current build prints `OLDEST VERSION`; an older one a bare number, which
/// accepts only that. Parsed by [`parse_wire`], the rule far machines get.
pub fn ask_wire_range(bin: &std::path::Path) -> Option<std::result::Result<(u32, u32), String>> {
    let out = Command::new(bin).arg("wire-version").stderr(Stdio::null()).output().ok()?;
    if !out.status.success() {
        return None;
    }
    let text = String::from_utf8_lossy(&out.stdout).trim().to_string();
    Some(parse_wire(&text).ok_or(text))
}

/// `compound-sup wire-version` output → the range it accepts, `(OLDEST,
/// VERSION)`. A current build prints `OLDEST VERSION` (`9 14`); an older build
/// prints a bare number, which is the range `VERSION VERSION`. Anything else
/// (a word, a third token, OLDEST above VERSION) is no answer.
pub fn parse_wire(out: &str) -> Option<(u32, u32)> {
    let toks: Vec<u32> = out.split_whitespace().map(|t| t.parse().ok()).collect::<Option<_>>()?;
    match toks[..] {
        [v] => Some((v, v)),
        [o, v] if o <= v => Some((o, v)),
        _ => None,
    }
}

pub fn dial(socket: &Path, timeout: Duration) -> Result<UnixStream> {
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

pub fn send(stream: &mut UnixStream, body: Value) -> std::io::Result<()> {
    let mut line = json!({ "ver": WIRE_VERSION }).as_object().unwrap().clone();
    line.extend(body.as_object().cloned().unwrap_or_default());
    let mut bytes = serde_json::to_vec(&line)?;
    bytes.push(b'\n');
    stream.write_all(&bytes)
}

/// One reply line, or `None` on EOF/timeout — a compound session that
/// answers nothing within the read timeout is read the same way a `tell`
/// with no watcher already reads: as `Unconfirmed`, not as a hang.
pub fn recv_line(stream: &mut UnixStream, timeout: Duration) -> Option<Value> {
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
pub fn b64(data: &[u8]) -> String {
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

/// Type at a session and press Enter — `Place::tell`, stripped of the
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
pub fn type_and_submit(socket: &Path, text: &str) -> Result<()> {
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
pub fn deliver_with(sup: &Path, session: &Path, id: &str, text: &str, wait: Duration, seat: &Seat) -> Result<Delivery> {
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
                DELIVERY_PENDING,
                wait.as_secs()
            )));
        }
    };
    let (stdout, stderr) = (String::from_utf8_lossy(&out.stdout), String::from_utf8_lossy(&out.stderr));
    read_deliver(&stdout, &stderr, out.status.success(), id, seat)
}

/// The answer to a `deliver`, as a function of what it printed.
pub fn read_deliver(stdout: &str, stderr: &str, ok: bool, id: &str, seat: &Seat) -> Result<Delivery> {
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
pub fn press_enter(stream: &mut UnixStream) -> Result<()> {
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
pub fn sup_command(sup: &Path, spawn: &Spawn<'_>) -> Command {
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
    crate::test_sups::tie_to_this_test(&mut cmd);
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
    for k in shed_keys() {
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

/// A `ps` table whose last column is `comm=` and first `pid=`, with each row's
/// `comm` made the name the process was run under: its `argv[0]`, out of
/// `cmdline` (NUL-separated).
///
/// macOS's `ps -o comm=` already answers that, the full path or the login
/// shell's `-zsh`; Linux's answers the kernel's 15-byte task name, which drops
/// the directory, and the callers ask the path (a finished tree's own binary
/// is one whose executable lives under the tree). A row whose `cmdline` is
/// empty or unreadable (a kernel thread, a process gone since `ps` ran) keeps
/// what `ps` said. Any other column list is answered unchanged.
fn comm_as_run(table: &str, columns: &str, cmdline: impl Fn(u32) -> Option<Vec<u8>>) -> String {
    let cols: Vec<&str> = columns.split(',').map(str::trim).collect();
    if cols.len() < 2 || cols.first() != Some(&"pid=") || cols.last() != Some(&"comm=") {
        return table.to_string();
    }
    let before = cols.len() - 1;
    let mut out = String::with_capacity(table.len());
    for line in table.lines() {
        let fields: Vec<&str> = line.split_whitespace().collect();
        let argv0 = fields
            .first()
            .and_then(|p| p.parse::<u32>().ok())
            .filter(|_| fields.len() > before)
            .and_then(&cmdline)
            .and_then(|b| b.split(|c| *c == 0).next().map(|a| String::from_utf8_lossy(a).into_owned()))
            .filter(|a| !a.is_empty());
        match argv0 {
            Some(a) => {
                out.push_str(&fields[..before].join(" "));
                out.push(' ');
                out.push_str(&a);
            }
            None => out.push_str(line),
        }
        out.push('\n');
    }
    out
}

#[cfg(test)]
mod comm_tests {
    use super::*;

    /// `pid_runs` answers as `kill -0` does, by syscall: this process runs, a
    /// reaped child does not, and pid 0 (this process's group to kill(2)) and
    /// pids past `i32` are no process.
    #[test]
    fn a_pid_runs_by_syscall_as_kill_zero_answers() {
        assert!(pid_runs(std::process::id()));
        let mut child = std::process::Command::new("true").spawn().unwrap();
        let gone = child.id();
        child.wait().unwrap();
        assert!(!pid_runs(gone), "a reaped child does not run");
        assert!(!pid_runs(0));
        assert!(!pid_runs(u32::MAX));
    }

    #[test]
    fn comm_is_the_name_a_process_was_run_under() {
        let cmdline = |pid: u32| match pid {
            10 => Some(b"/w/.worktrees/r/target/debug/stand\x0060\x00".to_vec()),
            11 => Some(b"-bash\x00".to_vec()),
            _ => Some(Vec::new()),
        };
        let ps = "   10     1 stand\n   11    10 bash\n    2     0 kthreadd\n";
        assert_eq!(
            comm_as_run(ps, "pid=,ppid=,comm=", cmdline),
            "10 1 /w/.worktrees/r/target/debug/stand\n11 10 -bash\n    2     0 kthreadd\n"
        );
        assert_eq!(comm_as_run(ps, "pid=,command=", cmdline), ps, "only a trailing comm= is read again");
    }
}

/// Ends the pane of a seat whose agent has left: the supervisor, its shell and
/// the renderer, a few seconds on so the agent finishes its own exit. Detached
/// and in its own group, because the hook that calls it runs under the very
/// group it ends. The manager's `seat.stopped` is the backstop for an ending
/// that never said `SessionEnd` (wsp-030 deleted the daemon's `ended_panes` sweep).
pub fn close_pane_soon(sup: u32) {
    if sup == std::process::id() {
        return;
    }
    let script = format!("sleep {CLOSE_AFTER_SECS}; kill -s TERM -- -{sup}; sleep 2; kill -s KILL -- -{sup}");
    let _ = Command::new("sh")
        .args(["-c", &script])
        .process_group(0)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn();
}

/// Seconds an exited agent's pane stays before it is closed.
pub const CLOSE_AFTER_SECS: u32 = 2;

/// The forwards the seats on `machine` should have, as `(local socket, far
/// socket)`, read from the seat records. What the tunnel supervisor re-adds when
/// The newest modification time among a seat directory and the files in it,
/// in epoch seconds — `Place::quiet_since` for both backends
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
        // The transcript's path, so a reader after the seat is gone finds it
        // without searching for the session id (`wsp-066`).
        "transcript": str_of(&rec, "transcript"),
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
/// `Supervisor` therefore read `seats/`, and on a machine where every seat is
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
/// the reason [`crate::shed::CHILD_MARKER`] carries: a session that inherits it writes
/// no transcript at all, and the only evidence is one truncated line in a pane
/// this backend does not have.
pub fn child_env(cmd: &mut Command, seat: &Seat, env: &BTreeMap<String, String>) {
    for (k, v) in env {
        match v.is_empty() {
            true => cmd.env_remove(k),
            false => cmd.env(k, v),
        };
    }
    for k in shed_keys() {
        cmd.env_remove(k);
    }
    // Last, and not from the order: the seat's own name is the one thing wsp
    // could not have put in `Order::env`, because it did not exist yet. This is
    // the direction `place.rs` reversed — the backend delivers the seat to
    // whatever runs in it, and says under what name.
    cmd.env(SEAT_ENV, seat.as_str());
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
/// Written for `heard`, which every hook reaches; best-effort like everything
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

    let _ = write_atomic_into(
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
pub fn u_at(v: &Value, key: &str) -> u64 {
    v.get(key).and_then(|x| x.as_u64()).unwrap_or(0)
}

// ---- seat.did: transcripts, moved whole from `agent_commands` -------------
//
// Where a session was written and how to read it are the host's, because the
// transcript is on the host's disk (host-api's `FileRef`: wsp reads it through
// the manager, never by opening the path). What a transcript *means* — what
// ran, what it cost, what the turn did — stays with wsp's parsers, which take
// what is read here.

/// An opencode session id as opencode spells it: `ses_` and alphanumerics.
/// Checked before it reaches a command line; anything else is refused.
pub fn opencode_id(session: &str) -> bool {
    !session.is_empty() && session.chars().all(|c| c.is_ascii_alphanumeric() || c == '_')
}

/// The last `bytes` of a file, from the first whole line in them.
fn tail_of(file: &Path, bytes: u64) -> Option<String> {
    use std::io::{Read, Seek, SeekFrom};
    let mut f = fs::File::open(file).ok()?;
    let len = f.metadata().map(|m| m.len()).unwrap_or(0);
    let from = len.saturating_sub(bytes);
    f.seek(SeekFrom::Start(from)).ok()?;
    let mut read = Vec::new();
    f.take(bytes).read_to_end(&mut read).ok()?;
    let mut text = String::from_utf8_lossy(&read).into_owned();
    // The first line is cut in the middle when the read started inside the file.
    if from > 0 {
        let i = text.find('\n')?;
        text.drain(..=i);
    }
    Some(text)
}

/// opencode, wherever it is — the same reasoning as `claude_bin` (`agent_commands`), and one
/// extra reason of its own.
///
/// `~/.opencode/bin` reached `PATH` through `.zshrc`, which a **non-interactive**
/// shell does not read, so `Command::new("opencode")` failed from a hook or a
/// script on a machine that plainly had it. That was fixed at the source by
/// moving the export to `~/.zshenv` on 2026-08-21, and this stays anyway: the
/// lookup costs nothing and the failure it prevents reads as "opencode is not
/// installed" on a machine where it is.
pub fn opencode_bin() -> PathBuf {
    if let Some(v) = std::env::var_os("OPENCODE_BIN") {
        let p = PathBuf::from(v);
        if !p.as_os_str().is_empty() {
            return p;
        }
    }
    if let Some(paths) = std::env::var_os("PATH") {
        for dir in std::env::split_paths(&paths) {
            let p = dir.join("opencode");
            if p.is_file() {
                return p;
            }
        }
    }
    util::home().join(".opencode/bin/opencode")
}

/// Everything one of these runtimes wrote to stdout, through a file rather
/// than a pipe.
///
/// **The pipe was losing most of it, and saying nothing.** `opencode export
/// --sanitize` on a 53-turn session writes 165,230 bytes. Asked for down a
/// pipe, wsp received 65,536 of them — one pipe buffer — and an exit status of
/// *zero*, so `read_export` (`agent_commands`) was handed a JSON document cut off mid-string,
/// returned nothing, and `ui-007` closed with no cost clause at all. That is
/// `core-039`. It was read at first as `despawn` ending the agent before its
/// runtime could be asked. Driven in a sandbox, both endings wrote the clause —
/// `wsp release` on a live opencode and `wsp despawn` on another — and then the
/// row that had lost one lost it again on a replay through `release`, with no
/// agent left to kill. Nothing about the ending is the variable. The size of
/// the export is.
///
/// The cause belongs to the runtime rather than to the subcommand: these are
/// Bun-compiled binaries, their stdout writes are asynchronous, and the process
/// exits without waiting for a pipe it has filled to drain. A file has no
/// buffer to overrun and its writes complete before exit — the same export
/// redirected to one came back 165,230 bytes three times out of three.
///
/// So every capture in this file goes through here, including the two that are
/// small today: `opencode models` is 13KB and `claude agents --json` is 1.4KB,
/// both grow with what is on the machine, and both fail plausibly rather than
/// loudly when cut — a valid model refused, an agent that cannot be addressed.
/// The defence is one temp file, and waiting to be the second row this costs is
/// not a saving.
///
/// The file is this process's own, is opened `0600` because a sanitized export
/// still carries the shape of one attempt and a temp directory is shared, and
/// is removed however this ends.
pub fn captured(bin: PathBuf, args: &[&str]) -> std::result::Result<Vec<u8>, String> {
    // Named as the reader would type it — `opencode models`, not the resolved
    // path — because these strings reach a person inside a refusal that is
    // about their `--model`, not about where wsp found the binary.
    let said = format!(
        "{} {}",
        bin.file_name().unwrap_or(bin.as_os_str()).to_string_lossy(),
        args.join(" ")
    );
    // Unique per call and not merely per process. Nothing here runs two at
    // once today, and a name that would silently collide the day something
    // does is a trap laid for a reader with no reason to look here.
    static NTH: AtomicU64 = AtomicU64::new(0);
    let at = std::env::temp_dir().join(format!(
        "wsp-captured-{}-{}",
        std::process::id(),
        NTH.fetch_add(1, Ordering::Relaxed)
    ));
    let sink = std::fs::OpenOptions::new()
        .write(true)
        .create(true)
        .truncate(true)
        .mode(0o600)
        .open(&at)
        .map_err(|e| format!("{said}: {e}"))?;
    let ran = Command::new(&bin)
        .args(args)
        .stdin(Stdio::null())
        .stdout(Stdio::from(sink))
        .stderr(Stdio::null())
        .status();
    let out = match ran {
        Err(e) => Err(format!("{said}: {e}")),
        Ok(s) if !s.success() => Err(format!("{said} failed")),
        Ok(_) => std::fs::read(&at).map_err(|e| format!("{said}: {e}")),
    };
    // Whatever happened, including the failures: a capture that could not be
    // read is exactly the one whose leftovers nobody comes back for.
    let _ = std::fs::remove_file(&at);
    out
}

/// `opencode db`'s rows for one session, newest first.
///
/// **`opencode db` and not a sqlite crate.** `serde_json` is the whole of this
/// tree's dependency list and it stays that way, and opencode ships the reader:
/// `opencode db <query> --format json` prints rows as JSON, so the query is
/// `select` over the `message` table with `json_extract` on the `data` column
/// rather than a schema this repository has to be right about.
pub fn opencode_rows(session: &str) -> Option<Vec<Value>> {
    let out = Command::new(opencode_bin())
        .args([
            "db",
            &format!(
                "select json_extract(data,'$.role') as role, \
                 json_extract(data,'$.time.created') as created, \
                 json_extract(data,'$.time.completed') as completed \
                 from message where session_id = '{session}' order by time_created desc limit 2"
            ),
            "--format",
            "json",
        ])
        .stdin(Stdio::null())
        .output()
        .ok()?;
    if !out.status.success() {
        return None;
    }
    serde_json::from_slice(&out.stdout).ok()
}

/// The directory name pi derives from a cwd for its session store — the
/// "mangling" pi's session-manager.js applies.
///
/// pi resolves the cwd, strips one leading `/` or `\`, and replaces every
/// `/`, `\`, `:` with `-`, wrapped in `--...--`. This function is the pure
/// half of that: given a canonical cwd, produce the directory name pi would
/// store sessions under.
pub fn pi_session_dir_name(cwd: &str) -> String {
    let stripped = cwd.strip_prefix('/').or_else(|| cwd.strip_prefix('\\')).unwrap_or(cwd);
    let mangled: String = stripped.chars().map(|c| match c {
        '/' | '\\' | ':' => '-',
        other => other,
    }).collect();
    format!("--{}--", mangled)
}

/// The session file for a pi session id under a cwd, or `None` if the id is
/// invalid or no file is found.
///
/// The id is validated before it reaches a path — it arrives from the backend
/// and must not be used to open arbitrary files. It must be non-empty and
/// contain only alphanumerics, `-`, `_`, `.`.
pub fn pi_session_file(session: &str, cwd: &str) -> Option<PathBuf> {
    if session.is_empty() || !session.chars().all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_' || c == '.') {
        return None;
    }
    let agent_dir = std::env::var_os("PI_CODING_AGENT_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(|| util::home().join(".pi").join("agent"));
    let dir_name = if let Ok(canonical) = std::fs::canonicalize(cwd) {
        pi_session_dir_name(&canonical.to_string_lossy())
    } else {
        pi_session_dir_name(cwd)
    };
    let sessions = agent_dir.join("sessions").join(&dir_name);
    let suffix = format!("_{session}.jsonl");
    let mut best: Option<(String, PathBuf)> = None;
    for entry in std::fs::read_dir(&sessions).ok()?.flatten() {
        let name = entry.file_name();
        let name = name.to_string_lossy();
        if name.ends_with(&suffix) {
            if best.as_ref().is_none_or(|(prev, _)| name.as_ref() > prev.as_str()) {
                let ts = name.strip_suffix(&suffix).unwrap_or(&name).to_string();
                best = Some((ts, entry.path()));
            }
        }
    }
    best.map(|(_, p)| p)
}

/// The file the session was written to, most likely place first.
///
/// Claude Code names the directory after the cwd with every character that is
/// not a letter, a digit or a hyphen replaced by one — `/Users/ed/.local` is
/// `-Users-ed--local` — so the direct hit costs one `stat` and is what happens
/// every time an agent stayed in the tree it was spawned into.
///
/// **The scan is not belt and braces.** A claim's cwd is where the *work* is
/// and the transcript is named after where the *pane* was, `wsp resume` starts
/// a session in a new workspace at a recorded cwd, and `wsp checkout` is free
/// to move a worktree; every one of those makes the derived path wrong while
/// the session id stays exactly right. A session id is a uuid, so a scan
/// cannot find the wrong file — it can only fail to find one.
///
/// The id is checked before it is put in a path. It arrives from the backend,
/// which got it from the agent, and `spawn` is a verb agents drive: a session id with
/// a `/` in it is not a session id, and refusing it here is cheaper than
/// discovering what it opened.
pub fn claude_transcript(session: &str, cwd: &str) -> Option<PathBuf> {
    if session.is_empty() || !session.chars().all(|c| c.is_ascii_alphanumeric() || c == '-') {
        return None;
    }
    let file = format!("{session}.jsonl");
    let projects = util::home().join(".claude").join("projects");
    let derived = projects.join(mangle(cwd)).join(&file);
    if derived.is_file() {
        return Some(derived);
    }
    std::fs::read_dir(&projects)
        .ok()?
        .filter_map(|e| e.ok())
        .map(|e| e.path().join(&file))
        .find(|p| p.is_file())
}

/// Every Claude Code transcript written in a tree whose path ends in `tree`
/// (`/.worktrees/wsp-313`): the `*.jsonl` files of each project directory whose
/// name ends in that tail as [`mangle`] spells it. The tail and not the whole
/// path, because a tree may have moved since, or sat under another checkout;
/// a tail that is empty finds nothing rather than every project.
pub fn claude_transcripts_of_tree(tree: &str) -> Vec<PathBuf> {
    let tail = mangle(tree);
    if tail.trim_matches('-').is_empty() {
        return Vec::new();
    }
    let projects = util::home().join(".claude").join("projects");
    let mut out = Vec::new();
    for dir in fs::read_dir(&projects).into_iter().flatten().flatten() {
        if !dir.file_name().to_string_lossy().ends_with(&tail) {
            continue;
        }
        for f in fs::read_dir(dir.path()).into_iter().flatten().flatten() {
            if f.path().extension().is_some_and(|x| x == "jsonl") {
                out.push(f.path());
            }
        }
    }
    out.sort();
    out
}

/// A cwd as Claude Code spells it in a directory name.
pub fn mangle(cwd: &str) -> String {
    cwd.chars().map(|c| if c.is_ascii_alphanumeric() || c == '-' { c } else { '-' }).collect()
}


// ---- moved down from the wire with the code that uses them (wsp-020) -----

/// A file replaced whole: written beside it and renamed over it, so a reader
/// never sees half a record, and synced first. The store's `write_atomic`
/// does the same; this one does not count the write toward the store's own
/// `steady` passes, which walk the task, project and list directories and
/// never a seat's.
pub(crate) fn write_atomic(path: &Path, contents: &str) -> std::io::Result<()> {
    fs::create_dir_all(path.parent().unwrap_or(Path::new(".")))?;
    write_atomic_into(path, contents)
}

/// [`write_atomic`] into a directory that must already be there, and is not
/// made: a seat's, written by a hook (`wsp-092`). A hook that checked the
/// seat's directory and then lost the race with its end must not make the
/// directory again, which would stand as a seat with no record in every
/// census after. The write fails instead, as a hook's write may.
pub(crate) fn write_atomic_into(path: &Path, contents: &str) -> std::io::Result<()> {
    let dir = path.parent().unwrap_or(Path::new("."));
    let tmp = dir.join(format!(
        ".{}.{}.tmp",
        path.file_name().and_then(|x| x.to_str()).unwrap_or("wsp"),
        std::process::id()
    ));
    {
        let mut f = fs::File::create(&tmp)?;
        f.write_all(contents.as_bytes())?;
        f.sync_all()?;
    }
    fs::rename(&tmp, path)
}

/// `crates/supervisor/src/proto.rs` VERSION at the time this was written.
/// Bumped there without a matching bump here is exactly the drift
/// `robustness-017` warns an adapter not to risk quietly — so a mismatch is
/// refused rather than sent into a peer that may not parse it.
///
/// **That drift happened, 2026-09-22, and this is what it looks like.**
/// `compound-031` bumped the supervisor to v7 for `Body::Close`, correctly
/// re-reading `proto.rs` at rebase exactly as its decision told it to — but
/// this constant is in the OTHER repository, so nothing it could read named
/// it. The first symptom was `wsp tell` refused with `unsupported ver 6`,
/// which reads as a fault in the seat rather than in a hand-copied number.
/// v7 is purely additive (`Close {}` added, no shape changed), so every
/// message wsp already sends stays valid at it and this is a one-line catch-up
/// rather than a port.
///
/// The duplication itself is `compound-026`, which is where it should be
/// fixed: a constant maintained by hand in two repositories will drift again,
/// and the only reason this cost minutes rather than a day is that the
/// supervisor refuses a version it does not know instead of guessing.
///
/// **v8, `compound-101`'s split, caught the same way (`compound-136`).**
/// This time the drift was worse than a stale number: `compound-101` moved
/// `screen`/`state` off `compound-sup` entirely, onto a second binary
/// (`compound-render`) answering a second socket (`<name>.rsock`) that this
/// file did not know existed. A wire-version-aware wsp calling a verb that
/// had moved would have failed exactly the same way a stale `WIRE_VERSION`
/// does — refused loudly, but for the wrong stated reason — so this bump
/// came with `crate::host::compound_render_binary`, `Compound::render_socket_of` and
/// every call site that used to dial `compound-sup` for a screen-shaped
/// answer moving to dial `compound-render` instead. v8 is *not* purely
/// additive the way v7 was: it is a new socket with a smaller `Body` on the
/// old one, which is exactly the "whole shape" `compound` d2 says a version
/// bump claims, and every caller in this file was re-checked against it
/// rather than assumed compatible.
pub const WIRE_VERSION: u32 = 9;

/// The words a delivery that did not finish in time opens its refusal with: what a
/// caller that only sees the text (`wsp tell`'s stderr) reads as "asked again, not failed".
pub const DELIVERY_PENDING: &str = "delivery still pending";

/// Whether compound can tell what an agent of this kind is doing — the kinds
/// the ported engine has a manifest for (`compound-109`).
///
/// **A function and not the `matches!` it was, because a second reader needs
/// it.** A governor reseated on a kind compound cannot read would start, read
/// `Unknown` for ever, and never be confirmed as seated — the
/// `tokenhub-spec-sync` shape, a pane that never comes up paid for every
/// twenty minutes. `crate::repair` asks this before it spends anything.
pub fn reads_kind(kind: &str) -> bool {
    matches!(kind, "claude" | "opencode" | "pi")
}

/// A file read as JSON, or an empty object — a seat whose record is missing is
/// the caller's question rather than this function's.
pub fn read_json(path: &PathBuf) -> Value {
    fs::read_to_string(path).ok().and_then(|s| serde_json::from_str(&s).ok()).unwrap_or(json!({}))
}

pub fn str_of(v: &Value, key: &str) -> String {
    v.get(key).and_then(|x| x.as_str()).unwrap_or("").to_string()
}
