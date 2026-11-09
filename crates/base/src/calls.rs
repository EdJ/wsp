//! What a wsp call costs, written down (`wsp-481`).
//!
//! Ed, 2026-11-07, after `wip --json` was measured by hand at 12.6 s: "add a
//! feature to wsp to record the times of its calls, so we can start to optimize
//! those". Nothing recorded it before; wsp-480 was found with `/usr/bin/time`
//! and `sample` against a binary with no symbols.
//!
//! **Two files, polls apart from actions** (`wsp-488`) — see [`Log`]: a poll
//! (a caller that says `WSP_POLL=1`, or a read-only verb run by no seat) goes to
//! `polls.jsonl`, everything else and the daemon's ticks to `calls.jsonl`, each
//! with its own cap and one rotation, so a poll storm cannot push the actions
//! out of the window.
//!
//! **Two kinds of line** — `$WSP_STATE/calls.jsonl`, appended with
//! `O_APPEND` and one `write`, so no lock is taken and two processes cannot
//! interleave inside a line (a line is far under `PIPE_BUF`-sized atomic
//! appends in practice, and a torn line is skipped by the reader):
//!
//! * `"k":"call"` — one per `wsp` process, written when it exits: verb and
//!   sub-verb, flag *names* (never values, prose or ids), the caller, wall ms,
//!   user and sys CPU ms for itself and for its children, the children by
//!   program, the exit code.
//! * `"k":"tick"` — one per daemon tick, with ms per named phase and the
//!   children that tick ran.
//!
//! **Children come from the one wrapper.** [`Command`] is a drop-in for
//! `std::process::Command` that times `output`, `status` and (at `wait`) a
//! spawned child, and adds it to the process-wide accumulator under the
//! program's file name. `ps`, `lsof`, `git`, `herdr`, `cargo` show up by name
//! with count and total wall ms; a straggler that still uses
//! `std::process::Command` is invisible to the split but not to the CPU totals,
//! which come from `getrusage` and miss nothing.
//!
//! **Cheap.** The cost on a call is one `getrusage` pair, one `proc_name`, one
//! small file read when run inside a seat (the governors record, to say
//! primary or governor), one `stat` and one `write`. `WSP_NO_CALLS=1` turns all
//! of it off. Each file rotates to `<name>.1.jsonl` past its cap ([`CAP`],
//! [`POLL_CAP`]).

use std::collections::BTreeMap;
use std::ffi::OsStr;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicI32, Ordering};
use std::sync::Mutex;
use std::time::{Duration, Instant};

use serde_json::{json, Value};

/// Rotate `calls.jsonl` to `calls.1.jsonl` when it is past this.
pub const CAP: u64 = 20 * 1024 * 1024;
/// The same for `polls.jsonl`. A line per poll, unfolded: see `docs/calls.md`.
pub const POLL_CAP: u64 = 20 * 1024 * 1024;
pub const FILE: &str = "calls.jsonl";
pub const OLD: &str = "calls.1.jsonl";
pub const POLL_FILE: &str = "polls.jsonl";
pub const POLL_OLD: &str = "polls.1.jsonl";

/// Which of the two logs a line belongs in.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Log {
    /// Calls that are acts, and the daemon's ticks: read one by one.
    Actions,
    /// Calls that are polls: read as a rate and a total per caller.
    Polls,
}

impl Log {
    pub fn file(self) -> &'static str {
        match self {
            Log::Actions => FILE,
            Log::Polls => POLL_FILE,
        }
    }
    pub fn old(self) -> &'static str {
        match self {
            Log::Actions => OLD,
            Log::Polls => POLL_OLD,
        }
    }
    pub fn cap(self) -> u64 {
        match self {
            Log::Actions => CAP,
            Log::Polls => POLL_CAP,
        }
    }
}

/// Which log a finished call goes to. A poll is a call whose caller says so
/// (`WSP_POLL=1`), or a read-only verb (`readonly` — the table is in
/// `src/readonly.rs`) run by no seat (`role == "none"`).
pub fn log_for(poll_env: bool, readonly: bool, role: &str) -> Log {
    if poll_env || (readonly && role == "none") {
        Log::Polls
    } else {
        Log::Actions
    }
}

fn poll_env() -> bool {
    std::env::var_os("WSP_POLL").is_some_and(|v| !v.is_empty() && v != "0")
}

static READ_ONLY: AtomicBool = AtomicBool::new(false);

/// Say that this process's verb writes no store state (decided in `main` from
/// the table in `src/readonly.rs`); with no seat around it, its call is a poll.
pub fn set_read_only(yes: bool) {
    READ_ONLY.store(yes, Ordering::SeqCst);
}

// ---------------------------------------------------------------------------
// The system calls. Declared here, as `util` does for `localtime_r`, so the
// crate stays dependency-free.

#[repr(C)]
#[derive(Clone, Copy, Default)]
struct Timeval {
    sec: i64,
    #[cfg(target_os = "macos")]
    usec: i32,
    #[cfg(target_os = "macos")]
    _pad: i32,
    #[cfg(not(target_os = "macos"))]
    usec: i64,
}

#[repr(C)]
#[derive(Clone, Copy, Default)]
struct Rusage {
    utime: Timeval,
    stime: Timeval,
    rest: [i64; 14],
}

extern "C" {
    fn getrusage(who: i32, usage: *mut Rusage) -> i32;
    fn getppid() -> i32;
    fn atexit(cb: extern "C" fn()) -> i32;
    #[cfg(target_os = "macos")]
    fn proc_name(pid: i32, buf: *mut u8, size: u32) -> i32;
}

/// User and sys CPU in microseconds: `(user, sys)`.
fn cpu(who: i32) -> (u64, u64) {
    let mut r = Rusage::default();
    // SAFETY: `r` is a correctly laid-out, writable `struct rusage`.
    if unsafe { getrusage(who, &mut r) } != 0 {
        return (0, 0);
    }
    let us = |t: Timeval| (t.sec.max(0) as u64) * 1_000_000 + (t.usec.max(0) as u64);
    (us(r.utime), us(r.stime))
}

fn own_cpu() -> (u64, u64) {
    cpu(0)
}

fn children_cpu() -> (u64, u64) {
    cpu(-1)
}

/// The command name of a process, as the kernel keeps it. No child process:
/// `ps -o comm=` would cost more than everything else here put together.
fn process_name(pid: i32) -> String {
    #[cfg(target_os = "macos")]
    {
        let mut buf = [0u8; 64];
        // SAFETY: `buf` is writable for the length passed.
        let n = unsafe { proc_name(pid, buf.as_mut_ptr(), buf.len() as u32) };
        if n > 0 {
            return String::from_utf8_lossy(&buf[..n as usize]).into_owned();
        }
        String::new()
    }
    #[cfg(not(target_os = "macos"))]
    {
        std::fs::read_to_string(format!("/proc/{pid}/comm")).map(|s| s.trim().to_string()).unwrap_or_default()
    }
}

// ---------------------------------------------------------------------------
// Children.

/// Count and total wall milliseconds per program, since the last drain.
#[derive(Default, Clone, Debug, PartialEq)]
pub struct Kids(pub BTreeMap<String, (u64, f64)>);

impl Kids {
    pub fn json(&self) -> Value {
        let mut m = serde_json::Map::new();
        for (p, (n, ms)) in &self.0 {
            m.insert(p.clone(), json!({"n": n, "ms": round1(*ms)}));
        }
        Value::Object(m)
    }
}

static KIDS: Mutex<Kids> = Mutex::new(Kids(BTreeMap::new()));

fn kids() -> std::sync::MutexGuard<'static, Kids> {
    KIDS.lock().unwrap_or_else(|e| e.into_inner())
}

fn round1(x: f64) -> f64 {
    (x * 10.0).round() / 10.0
}

/// The name a program goes under: its file name, not its path.
fn program_name(p: &OsStr) -> String {
    Path::new(p).file_name().unwrap_or(p).to_string_lossy().into_owned()
}

/// Record `n` runs of `program` taking `wall` between them. Public so that a
/// path that cannot use [`Command`] (a thread that waits on a child someone
/// else started) can still say what it cost.
pub fn note_child(program: &str, n: u64, wall: Duration) {
    let mut k = kids();
    let e = k.0.entry(program.to_string()).or_insert((0, 0.0));
    e.0 += n;
    e.1 += wall.as_secs_f64() * 1000.0;
}

/// Take what has accumulated.
pub fn drain_children() -> Kids {
    std::mem::take(&mut *kids())
}

/// A drop-in for `std::process::Command` that says what it ran and how long it
/// took. Only the methods wsp uses; a missing one is a compile error, not a
/// silent bypass — add it here.
pub struct Command {
    inner: std::process::Command,
    name: String,
}

impl Command {
    pub fn new<S: AsRef<OsStr>>(program: S) -> Command {
        Command { name: program_name(program.as_ref()), inner: std::process::Command::new(program) }
    }
    pub fn arg<S: AsRef<OsStr>>(&mut self, a: S) -> &mut Command {
        self.inner.arg(a);
        self
    }
    pub fn args<I, S>(&mut self, a: I) -> &mut Command
    where
        I: IntoIterator<Item = S>,
        S: AsRef<OsStr>,
    {
        self.inner.args(a);
        self
    }
    pub fn env<K: AsRef<OsStr>, V: AsRef<OsStr>>(&mut self, k: K, v: V) -> &mut Command {
        self.inner.env(k, v);
        self
    }
    pub fn envs<I, K, V>(&mut self, vars: I) -> &mut Command
    where
        I: IntoIterator<Item = (K, V)>,
        K: AsRef<OsStr>,
        V: AsRef<OsStr>,
    {
        self.inner.envs(vars);
        self
    }
    pub fn env_remove<K: AsRef<OsStr>>(&mut self, k: K) -> &mut Command {
        self.inner.env_remove(k);
        self
    }
    pub fn env_clear(&mut self) -> &mut Command {
        self.inner.env_clear();
        self
    }
    pub fn current_dir<P: AsRef<Path>>(&mut self, d: P) -> &mut Command {
        self.inner.current_dir(d);
        self
    }
    pub fn stdin<T: Into<std::process::Stdio>>(&mut self, c: T) -> &mut Command {
        self.inner.stdin(c);
        self
    }
    pub fn stdout<T: Into<std::process::Stdio>>(&mut self, c: T) -> &mut Command {
        self.inner.stdout(c);
        self
    }
    pub fn stderr<T: Into<std::process::Stdio>>(&mut self, c: T) -> &mut Command {
        self.inner.stderr(c);
        self
    }
    pub fn get_program(&self) -> &OsStr {
        self.inner.get_program()
    }
    pub fn get_args(&self) -> std::process::CommandArgs<'_> {
        self.inner.get_args()
    }
    pub fn get_current_dir(&self) -> Option<&Path> {
        self.inner.get_current_dir()
    }
    pub fn get_envs(&self) -> std::process::CommandEnvs<'_> {
        self.inner.get_envs()
    }

    // Unix extensions (`std::os::unix::process::CommandExt`, which is sealed
    // and so cannot be implemented here).
    pub fn process_group(&mut self, pgroup: i32) -> &mut Command {
        std::os::unix::process::CommandExt::process_group(&mut self.inner, pgroup);
        self
    }
    /// # Safety
    /// As `CommandExt::pre_exec`.
    pub unsafe fn pre_exec<F>(&mut self, f: F) -> &mut Command
    where
        F: FnMut() -> std::io::Result<()> + Send + Sync + 'static,
    {
        std::os::unix::process::CommandExt::pre_exec(&mut self.inner, f);
        self
    }
    /// Replace this process. Returns only on failure.
    pub fn exec(&mut self) -> std::io::Error {
        // The image is replaced, so the atexit line never runs; say what we
        // were doing first.
        finish_now();
        std::os::unix::process::CommandExt::exec(&mut self.inner)
    }

    pub fn output(&mut self) -> std::io::Result<std::process::Output> {
        let t = Instant::now();
        let r = self.inner.output();
        note_child(&self.name, 1, t.elapsed());
        r
    }
    pub fn status(&mut self) -> std::io::Result<std::process::ExitStatus> {
        let t = Instant::now();
        let r = self.inner.status();
        note_child(&self.name, 1, t.elapsed());
        r
    }
    /// The child is counted when it starts (with the time starting took) and
    /// its wall is added when it is waited on, so one that is never waited for
    /// still shows as a run.
    pub fn spawn(&mut self) -> std::io::Result<Child> {
        let t = Instant::now();
        let r = self.inner.spawn();
        note_child(&self.name, 1, t.elapsed());
        r.map(|c| Child { inner: c, name: self.name.clone(), started: Instant::now(), waited: false })
    }
}

impl std::fmt::Debug for Command {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        self.inner.fmt(f)
    }
}

/// A spawned child. Dereferences to `std::process::Child` for `id`, `kill`,
/// `stdin` and the rest; the waits are its own so the run is timed.
pub struct Child {
    inner: std::process::Child,
    name: String,
    started: Instant,
    waited: bool,
}

impl Child {
    fn ended(&mut self) {
        if !self.waited {
            self.waited = true;
            note_child(&self.name, 0, self.started.elapsed());
        }
    }
    pub fn wait(&mut self) -> std::io::Result<std::process::ExitStatus> {
        let r = self.inner.wait();
        self.ended();
        r
    }
    pub fn try_wait(&mut self) -> std::io::Result<Option<std::process::ExitStatus>> {
        let r = self.inner.try_wait();
        if matches!(r, Ok(Some(_))) {
            self.ended();
        }
        r
    }
    pub fn wait_with_output(self) -> std::io::Result<std::process::Output> {
        let Child { inner, name, started, waited } = self;
        let r = inner.wait_with_output();
        if !waited {
            note_child(&name, 0, started.elapsed());
        }
        r
    }
}

impl std::fmt::Debug for Child {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        self.inner.fmt(f)
    }
}

impl std::ops::Deref for Child {
    type Target = std::process::Child;
    fn deref(&self) -> &std::process::Child {
        &self.inner
    }
}

impl std::ops::DerefMut for Child {
    fn deref_mut(&mut self) -> &mut std::process::Child {
        &mut self.inner
    }
}

// ---------------------------------------------------------------------------
// Where it is written.

/// `$WSP_STATE`, or `~/.local/state/wsp`. Under a test build with no
/// `WSP_STATE` there is no answer: a test must never write the real file.
fn state_dir() -> Option<PathBuf> {
    match std::env::var_os("WSP_STATE") {
        Some(v) if !v.is_empty() => Some(PathBuf::from(v)),
        _ => {
            #[cfg(any(test, feature = "test-support"))]
            {
                None
            }
            #[cfg(not(any(test, feature = "test-support")))]
            {
                Some(crate::util::home().join(".local/state/wsp"))
            }
        }
    }
}

pub fn off() -> bool {
    std::env::var_os("WSP_NO_CALLS").is_some_and(|v| !v.is_empty() && v != "0")
}

/// Append one line to `dir/calls.jsonl`, rotating first if the file is past
/// `cap`. One `write` on an `O_APPEND` descriptor.
pub fn append(dir: &Path, line: &Value, cap: u64) {
    append_to(dir, Log::Actions, line, cap)
}

/// [`append`] to the named log.
pub fn append_to(dir: &Path, log: Log, line: &Value, cap: u64) {
    let mut s = line.to_string();
    s.push('\n');
    let path = dir.join(log.file());
    if std::fs::metadata(&path).is_ok_and(|m| m.len() >= cap) {
        let _ = std::fs::rename(&path, dir.join(log.old()));
    }
    let f = std::fs::OpenOptions::new().create(true).append(true).open(&path);
    if let Ok(mut f) = f {
        let _ = f.write_all(s.as_bytes());
    }
}

fn write(log: Log, line: Value) {
    if off() {
        return;
    }
    if let Some(dir) = state_dir() {
        if dir.is_dir() {
            append_to(&dir, log, &line, log.cap());
        }
    }
}

// ---------------------------------------------------------------------------
// The call.

/// What the process knew at its start.
struct Open {
    at: i64,
    started: Instant,
    verb: String,
    sub: String,
    flags: Vec<String>,
}

static OPEN: Mutex<Option<Open>> = Mutex::new(None);
static CODE: AtomicI32 = AtomicI32::new(i32::MIN);
static QUIET: AtomicBool = AtomicBool::new(false);

extern "C" fn at_exit() {
    finish_now();
}

/// Start the record for this process. Call once, early in `main`, with the verb,
/// its sub-verb (already reduced to a documented word, never an id or prose) and
/// the flag *names*.
pub fn begin(verb: &str, sub: &str, flags: Vec<String>) {
    if off() {
        return;
    }
    let mut o = OPEN.lock().unwrap_or_else(|e| e.into_inner());
    if o.is_some() {
        return;
    }
    *o = Some(Open {
        at: crate::util::epoch_secs(),
        started: Instant::now(),
        verb: verb.to_string(),
        sub: sub.to_string(),
        flags,
    });
    // SAFETY: a plain `extern "C" fn()`.
    unsafe {
        atexit(at_exit);
    }
}

/// For a process whose call line would mean nothing (the daemon: it writes tick
/// lines instead).
pub fn quiet() {
    QUIET.store(true, Ordering::SeqCst);
}

/// Say how this process is ending, for the line its exit writes.
pub fn set_code(code: i32) {
    CODE.store(code, Ordering::SeqCst);
}

/// `std::process::exit`, remembering the code for the record.
pub fn exit(code: i32) -> ! {
    set_code(code);
    std::process::exit(code)
}

/// The seat id, if this process is in a seat.
fn seat_id() -> Option<String> {
    ["WSP_SEAT_ID", "HERDR_PANE_ID"].iter().find_map(|k| std::env::var(k).ok().filter(|v| !v.is_empty()))
}

/// primary, governor, member, verifier, check, daemon or none — from the
/// environment wsp put there and the governors record, never from anything the
/// caller says.
fn role(seat: Option<&str>, verb: &str, state: Option<&Path>) -> &'static str {
    if verb == "daemon" {
        return "daemon";
    }
    let Some(seat) = seat else { return "none" };
    if let Some(dir) = state {
        // The record is tens of KB of history; a seat that is not named in it at
        // all (every member) is settled by one substring search, not a parse.
        let named = std::fs::read_to_string(dir.join("governors.json")).ok().filter(|t| t.contains(&format!("\"{seat}\"")));
        if let Some(text) = named {
            if let Ok(Value::Object(m)) = serde_json::from_str::<Value>(&text) {
                for (scope, rec) in &m {
                    if rec.get("pane").and_then(Value::as_str) == Some(seat) {
                        return if scope == "primary" { "primary" } else { "governor" };
                    }
                }
            }
        }
    }
    if let Some(v) = std::env::var_os("WSP_VERDICT") {
        let kind = Path::new(&v).parent().and_then(|p| p.file_name()).map(|s| s.to_string_lossy().into_owned());
        return match kind.as_deref() {
            Some("check") => "check",
            Some("verified") => "verifier",
            _ => "member",
        };
    }
    if std::env::var_os("WSP_TASK").is_some() {
        return "member";
    }
    "seat"
}

fn ms(us: u64) -> f64 {
    round1(us as f64 / 1000.0)
}

/// The line for a finished call. Pure, so a test can pin its shape.
#[allow(clippy::too_many_arguments)]
pub fn call_line(
    at: i64,
    verb: &str,
    sub: &str,
    flags: &[String],
    seat: Option<&str>,
    role: &str,
    parent: &str,
    wall: Duration,
    me: (u64, u64),
    kids_cpu: (u64, u64),
    kids: &Kids,
    code: Option<i32>,
) -> Value {
    json!({
        "k": "call",
        "t": crate::util::iso_at(at),
        "verb": verb,
        "sub": sub,
        "flags": flags,
        "seat": seat.unwrap_or(""),
        "role": role,
        "parent": parent,
        "wall_ms": round1(wall.as_secs_f64() * 1000.0),
        "user_ms": ms(me.0),
        "sys_ms": ms(me.1),
        "kids_user_ms": ms(kids_cpu.0),
        "kids_sys_ms": ms(kids_cpu.1),
        "children": kids.json(),
        "exit": code,
    })
}

fn finish_now() {
    let open = OPEN.lock().unwrap_or_else(|e| e.into_inner()).take();
    let Some(o) = open else { return };
    if QUIET.load(Ordering::SeqCst) || off() {
        return;
    }
    let wall = o.started.elapsed();
    let me = own_cpu();
    let kc = children_cpu();
    let seat = seat_id();
    let state = state_dir();
    let r = role(seat.as_deref(), &o.verb, state.as_deref());
    // SAFETY: no arguments.
    let parent = process_name(unsafe { getppid() });
    let code = CODE.load(Ordering::SeqCst);
    let line = call_line(
        o.at,
        &o.verb,
        &o.sub,
        &o.flags,
        seat.as_deref(),
        r,
        &parent,
        wall,
        me,
        kc,
        &drain_children(),
        (code != i32::MIN).then_some(code),
    );
    write(log_for(poll_env(), READ_ONLY.load(Ordering::SeqCst), r), line);
}

// ---------------------------------------------------------------------------
// The daemon's tick.

struct Tick {
    at: i64,
    started: Instant,
    cpu0: (u64, u64),
    kids0: (u64, u64),
    phase: Option<(String, Instant)>,
    phases: Vec<(String, f64)>,
}

static TICK: Mutex<Option<Tick>> = Mutex::new(None);

fn tick_guard() -> std::sync::MutexGuard<'static, Option<Tick>> {
    TICK.lock().unwrap_or_else(|e| e.into_inner())
}

/// Start a tick. Anything the daemon ran since the last one is dropped from the
/// children so the line is this tick's alone.
pub fn tick_begin() {
    if off() {
        return;
    }
    let _ = drain_children();
    *tick_guard() = Some(Tick {
        at: crate::util::epoch_secs(),
        started: Instant::now(),
        cpu0: own_cpu(),
        kids0: children_cpu(),
        phase: None,
        phases: Vec::new(),
    });
}

/// End the phase that is running, if any, and start `name`. A phase may be named
/// again (a loop); its times add.
pub fn phase(name: &str) {
    let mut g = tick_guard();
    let Some(t) = g.as_mut() else { return };
    close(t);
    t.phase = Some((name.to_string(), Instant::now()));
}

fn close(t: &mut Tick) {
    if let Some((n, at)) = t.phase.take() {
        let d = at.elapsed().as_secs_f64() * 1000.0;
        match t.phases.iter_mut().find(|(p, _)| *p == n) {
            Some(e) => e.1 += d,
            None => t.phases.push((n, d)),
        }
    }
}

/// The line for a finished tick. Pure.
pub fn tick_line(at: i64, wall: Duration, me: (u64, u64), kids_cpu: (u64, u64), phases: &[(String, f64)], kids: &Kids) -> Value {
    let mut p = serde_json::Map::new();
    for (n, d) in phases {
        p.insert(n.clone(), json!(round1(*d)));
    }
    json!({
        "k": "tick",
        "t": crate::util::iso_at(at),
        "wall_ms": round1(wall.as_secs_f64() * 1000.0),
        "user_ms": ms(me.0),
        "sys_ms": ms(me.1),
        "kids_user_ms": ms(kids_cpu.0),
        "kids_sys_ms": ms(kids_cpu.1),
        "phases": Value::Object(p),
        "children": kids.json(),
    })
}

/// Finish the tick and write its line.
pub fn tick_end() {
    let t = tick_guard().take();
    let Some(mut t) = t else { return };
    close(&mut t);
    let (u, s) = own_cpu();
    let (ku, ks) = children_cpu();
    let line = tick_line(
        t.at,
        t.started.elapsed(),
        (u.saturating_sub(t.cpu0.0), s.saturating_sub(t.cpu0.1)),
        (ku.saturating_sub(t.kids0.0), ks.saturating_sub(t.kids0.1)),
        &t.phases,
        &drain_children(),
    );
    write(Log::Actions, line);
}

/// Drop a tick that did nothing worth a line (the loop's early `continue`).
pub fn tick_discard() {
    *tick_guard() = None;
    let _ = drain_children();
}

// ---------------------------------------------------------------------------
// The report.

/// One row of `wsp calls`: a verb, a caller or a child program.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct Row {
    pub name: String,
    pub count: u64,
    pub walls: Vec<f64>,
    /// CPU in ms: the process's own plus its children's.
    pub cpu_ms: f64,
    /// What the calls in this row ran, by program: runs and total wall ms.
    pub kids: BTreeMap<String, (u64, f64)>,
}

impl Row {
    pub fn p(&self, q: f64) -> f64 {
        if self.walls.is_empty() {
            return 0.0;
        }
        let mut w = self.walls.clone();
        w.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
        let i = ((w.len() as f64 * q).ceil() as usize).clamp(1, w.len()) - 1;
        w[i]
    }
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub enum By {
    Verb,
    Caller,
    Child,
}

fn f(v: &Value, k: &str) -> f64 {
    v.get(k).and_then(Value::as_f64).unwrap_or(0.0)
}

fn s<'a>(v: &'a Value, k: &str) -> &'a str {
    v.get(k).and_then(Value::as_str).unwrap_or("")
}

/// The name a call goes under for each grouping.
pub fn verb_of(v: &Value) -> String {
    let (verb, sub) = (s(v, "verb"), s(v, "sub"));
    let mut name = if sub.is_empty() { verb.to_string() } else { format!("{verb} {sub}") };
    if v.get("flags").and_then(Value::as_array).is_some_and(|a| a.iter().any(|x| x == "json")) {
        name.push_str(" --json");
    }
    name
}

pub fn caller_of(v: &Value) -> String {
    let (role, parent) = (s(v, "role"), s(v, "parent"));
    match (role, parent) {
        (r, "") => r.to_string(),
        (r, p) => format!("{r} via {p}"),
    }
}

/// Group call lines at or after `since` (epoch secs) and rank by CPU.
/// Tick lines are not calls and are left out. `total_cpu_ms` is all CPU in the
/// window, for the share column.
pub fn summarize(lines: &[Value], since: i64, by: By) -> (Vec<Row>, f64) {
    let mut rows: BTreeMap<String, Row> = BTreeMap::new();
    let mut total = 0.0;
    for v in lines.iter().filter(|v| s(v, "k") == "call") {
        if crate::util::epoch_of(s(v, "t")) < since {
            continue;
        }
        let cpu = f(v, "user_ms") + f(v, "sys_ms") + f(v, "kids_user_ms") + f(v, "kids_sys_ms");
        total += cpu;
        match by {
            By::Verb | By::Caller => {
                let name = if by == By::Verb { verb_of(v) } else { caller_of(v) };
                let r = rows.entry(name.clone()).or_insert_with(|| Row { name, ..Default::default() });
                r.count += 1;
                r.walls.push(f(v, "wall_ms"));
                r.cpu_ms += cpu;
                if let Some(Value::Object(kids)) = v.get("children") {
                    for (prog, k) in kids {
                        let e = r.kids.entry(prog.clone()).or_insert((0, 0.0));
                        e.0 += k.get("n").and_then(Value::as_u64).unwrap_or(0);
                        e.1 += f(k, "ms");
                    }
                }
            }
            By::Child => {
                if let Some(Value::Object(kids)) = v.get("children") {
                    for (prog, k) in kids {
                        let r = rows.entry(prog.clone()).or_insert_with(|| Row { name: prog.clone(), ..Default::default() });
                        let n = k.get("n").and_then(Value::as_u64).unwrap_or(0);
                        r.count += n;
                        if n > 0 {
                            // The average wall of this call's runs of it, once per
                            // run, so p50 and p95 are per child run.
                            let each = f(k, "ms") / n as f64;
                            r.walls.extend(std::iter::repeat(each).take(n as usize));
                        }
                    }
                }
            }
        }
    }
    let mut rows: Vec<Row> = rows.into_values().collect();
    if by == By::Child {
        // A child's CPU is not recorded per program (`getrusage` has only the
        // total), so rank by total wall and say so in the header.
        rows.sort_by(|a, b| {
            let (x, y) = (a.walls.iter().sum::<f64>(), b.walls.iter().sum::<f64>());
            y.partial_cmp(&x).unwrap_or(std::cmp::Ordering::Equal)
        });
    } else {
        rows.sort_by(|a, b| b.cpu_ms.partial_cmp(&a.cpu_ms).unwrap_or(std::cmp::Ordering::Equal));
    }
    (rows, total)
}


/// The daemon's ticks in the window, as one row per phase ranked by total ms,
/// and the number of ticks. A row's `walls` are that phase's ms in each tick it
/// ran in; `tick` is the whole pass.
pub fn summarize_ticks(lines: &[Value], since: i64) -> (Vec<Row>, u64) {
    let mut rows: BTreeMap<String, Row> = BTreeMap::new();
    let mut ticks = 0;
    for v in lines.iter().filter(|v| s(v, "k") == "tick") {
        if crate::util::epoch_of(s(v, "t")) < since {
            continue;
        }
        ticks += 1;
        let mut put = |name: &str, d: f64, cpu: f64| {
            let r = rows.entry(name.to_string()).or_insert_with(|| Row { name: name.to_string(), ..Default::default() });
            r.count += 1;
            r.walls.push(d);
            r.cpu_ms += cpu;
        };
        put("(whole tick)", f(v, "wall_ms"), f(v, "user_ms") + f(v, "sys_ms") + f(v, "kids_user_ms") + f(v, "kids_sys_ms"));
        if let Some(Value::Object(ph)) = v.get("phases") {
            for (n, d) in ph {
                put(n, d.as_f64().unwrap_or(0.0), 0.0);
            }
        }
    }
    let mut rows: Vec<Row> = rows.into_values().collect();
    rows.sort_by(|a, b| {
        // The whole tick first, then the phases by total ms.
        let key = |r: &Row| (r.name != "(whole tick)", -r.walls.iter().sum::<f64>() as i64);
        key(a).cmp(&key(b))
    });
    (rows, ticks)
}

/// Read every line of `calls.1.jsonl` then `calls.jsonl` under `dir`, skipping
/// anything that does not parse (a torn line).
pub fn read_all(dir: &Path) -> Vec<Value> {
    read_log(dir, Log::Actions)
}

/// [`read_all`] for the named log.
pub fn read_log(dir: &Path, log: Log) -> Vec<Value> {
    let mut out = Vec::new();
    for name in [log.old(), log.file()] {
        if let Ok(text) = std::fs::read_to_string(dir.join(name)) {
            out.extend(text.lines().filter_map(|l| serde_json::from_str::<Value>(l).ok()));
        }
    }
    out
}


/// What the file actually covers, which is not what was asked for once it has
/// rotated, or been switched off for a while, or is new. `wsp calls --since 2d`
/// over a file that starts three hours ago would otherwise read as two days.
#[derive(Debug, Clone, PartialEq)]
pub struct Coverage {
    pub log: Log,
    /// Epoch seconds of the oldest and newest record read, if any.
    pub first: Option<i64>,
    pub last: Option<i64>,
    pub live_bytes: u64,
    pub old_bytes: u64,
}

impl Coverage {
    pub fn of(lines: &[Value], dir: &Path) -> Coverage {
        Coverage::of_log(lines, dir, Log::Actions)
    }

    pub fn of_log(lines: &[Value], dir: &Path, log: Log) -> Coverage {
        let times = lines.iter().map(|v| crate::util::epoch_of(s(v, "t"))).filter(|t| *t > 0);
        let size = |n: &str| std::fs::metadata(dir.join(n)).map(|m| m.len()).unwrap_or(0);
        Coverage {
            log,
            first: times.clone().min(),
            last: times.max(),
            live_bytes: size(log.file()),
            old_bytes: size(log.old()),
        }
    }

    /// Seconds from the oldest record to the newest.
    pub fn span(&self) -> i64 {
        match (self.first, self.last) {
            (Some(a), Some(b)) => (b - a).max(0),
            _ => 0,
        }
    }

    /// Whether the oldest record is newer than the start of the window asked
    /// for, so the figures describe less than the window says.
    pub fn short(&self, since: i64) -> bool {
        self.first.is_some_and(|f| f > since)
    }

    /// The line that says so. `asked` is the window in seconds, `since` its start.
    pub fn line(&self, asked: i64, since: i64) -> String {
        let (Some(a), Some(b)) = (self.first, self.last) else {
            return format!("{} has no records", self.log.file());
        };
        let size = |n: u64| format!("{:.1} MB", n as f64 / 1_048_576.0);
        let mut out = format!(
            "{} covers {} ({} to {}); {} {} + {} {}",
            self.log.file(),
            crate::util::duration_human(self.span()),
            crate::util::iso_at(a),
            crate::util::iso_at(b),
            self.log.file(),
            size(self.live_bytes),
            self.log.old(),
            size(self.old_bytes)
        );
        if self.short(since) {
            out.push_str(&format!(
                " — SHORT: asked for {}, the oldest record is {} old, so these figures cover less than the window",
                crate::util::duration_human(asked),
                crate::util::duration_human((crate::util::epoch_secs() - a).max(0)),
            ));
        }
        out
    }

    pub fn json(&self, since: i64) -> Value {
        json!({
            "log": self.log.file(),
            "first": self.first.map(crate::util::iso_at),
            "last": self.last.map(crate::util::iso_at),
            "span_s": self.span(),
            "live_bytes": self.live_bytes,
            "old_bytes": self.old_bytes,
            "short": self.short(since),
        })
    }
}

pub fn state_path() -> Option<PathBuf> {
    state_dir()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn call(verb: &str, sub: &str, flags: &[&str], role: &str, parent: &str, wall: f64, cpu: f64, kids: Value) -> Value {
        json!({"k":"call","t":"2026-11-07T10:00:00Z","verb":verb,"sub":sub,"flags":flags,"role":role,"parent":parent,
               "wall_ms":wall,"user_ms":cpu,"sys_ms":0.0,"kids_user_ms":0.0,"kids_sys_ms":0.0,"children":kids})
    }

    /// A child run through the wrapper is counted by program name, once per run,
    /// with its wall time; and a drain empties the accumulator.
    /// The accumulator is process-wide, so the tests that drain it take turns.
    static KIDS_TEST: Mutex<()> = Mutex::new(());

    #[test]
    fn a_wrapped_child_is_counted_by_its_program_name() {
        let _g = KIDS_TEST.lock().unwrap_or_else(|e| e.into_inner());
        let _ = drain_children();
        let out = Command::new("/bin/echo").arg("hi").output().unwrap();
        assert_eq!(out.stdout, b"hi\n");
        assert!(Command::new("echo").arg("x").status().unwrap().success());
        let mut c = Command::new("sleep").arg("0.05").spawn().unwrap();
        c.wait().unwrap();
        let k = drain_children();
        assert_eq!(k.0["echo"].0, 2, "{k:?}");
        assert_eq!(k.0["sleep"].0, 1, "a spawn counts once, not once at spawn and once at wait: {k:?}");
        assert!(k.0["sleep"].1 >= 40.0, "the wait adds the child's run: {k:?}");
        assert!(drain_children().0.is_empty());
    }

    /// `wait_with_output` consumes the child and must still count its wall.
    #[test]
    fn wait_with_output_still_times_the_child() {
        let _g = KIDS_TEST.lock().unwrap_or_else(|e| e.into_inner());
        let _ = drain_children();
        let c = Command::new("sh")
            .args(["-c", "sleep 0.05; echo ok"])
            .stdout(std::process::Stdio::piped())
            .spawn()
            .unwrap();
        let out = c.wait_with_output().unwrap();
        assert_eq!(out.stdout, b"ok\n");
        let k = drain_children();
        assert_eq!(k.0["sh"].0, 1);
        assert!(k.0["sh"].1 >= 40.0, "{k:?}");
    }

    /// The shape of the line is what the reader and the report depend on.
    #[test]
    fn a_call_line_has_every_field_and_no_values() {
        let mut k = Kids::default();
        k.0.insert("ps".into(), (3, 41.25));
        let v = call_line(
            crate::util::epoch_of("2026-11-07T10:40:00Z"),
            "wip",
            "",
            &["json".to_string()],
            Some("cpd-1"),
            "member",
            "claude",
            Duration::from_millis(250),
            (100_000, 20_000),
            (30_000, 5_000),
            &k,
            Some(0),
        );
        for key in ["k", "t", "verb", "sub", "flags", "seat", "role", "parent", "wall_ms", "user_ms", "sys_ms", "kids_user_ms", "kids_sys_ms", "children", "exit"] {
            assert!(v.get(key).is_some(), "missing {key}: {v}");
        }
        assert_eq!(v["wall_ms"], 250.0);
        assert_eq!(v["user_ms"], 100.0);
        assert_eq!(v["children"]["ps"]["n"], 3);
        assert_eq!(v["t"], "2026-11-07T10:40:00Z");
    }

    /// Driven past several caps: the live file stays under one cap, the
    /// previous generation is kept in `calls.1.jsonl`, anything older is gone
    /// from both, and the two together stay under two caps plus a line — which
    /// is the "never over ~40 MB" of the real 20 MiB cap.
    #[test]
    fn the_file_rotates_at_its_cap_and_drops_the_older_generation() {
        let dir = std::env::temp_dir().join(format!("wsp-calls-rot-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let cap = 4096u64;
        let pad = "x".repeat(200);
        let mut line_len = 0;
        let mut worst = 0;
        let n = 120; // ~28 KB written: seven caps' worth
        for i in 0..n {
            let line = json!({"k":"call","m":format!("m{i:04}"),"pad":pad});
            line_len = line.to_string().len() as u64 + 1;
            append(&dir, &line, cap);
            let size = |f: &str| std::fs::metadata(dir.join(f)).map(|m| m.len()).unwrap_or(0);
            let (live, old) = (size(FILE), size(OLD));
            assert!(live < cap + line_len, "live {live} past cap + one line at line {i}");
            worst = worst.max(live + old);
        }
        assert!(worst <= 2 * cap + line_len, "live + old reached {worst}, past 2 x cap + a line");
        let live = std::fs::read_to_string(dir.join(FILE)).unwrap();
        let old = std::fs::read_to_string(dir.join(OLD)).unwrap();
        let last = format!("m{:04}", n - 1);
        assert!(live.contains(&last), "the newest line is in the live file");
        assert!(!old.contains(&last) && !live.contains("m0000") && !old.contains("m0000"), "the first line is gone from both files");
        // The old generation is the one just before the live one: its newest
        // line is the line just before the live file's first.
        let first_live: usize = live.lines().next().unwrap().split("\"m\":\"m").nth(1).unwrap()[..4].parse().unwrap();
        assert!(old.contains(&format!("m{:04}", first_live - 1)), "calls.1.jsonl is the previous generation");
        assert!(read_all(&dir).iter().all(|v| v["k"] == "call"));
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// The log a call goes to: the caller's word, or a read-only verb run by no
    /// seat; nothing else.
    #[test]
    fn a_poll_is_the_callers_word_or_a_read_only_verb_run_by_no_seat() {
        assert_eq!(log_for(true, false, "member"), Log::Polls, "WSP_POLL=1 is a poll whoever says it");
        assert_eq!(log_for(false, true, "none"), Log::Polls, "wip --json from compound's host");
        assert_eq!(log_for(false, true, "member"), Log::Actions, "an agent reading is an agent working");
        assert_eq!(log_for(false, true, "seat"), Log::Actions);
        assert_eq!(log_for(false, false, "none"), Log::Actions, "a verb that writes is an act, seat or none");
        assert_eq!(log_for(false, false, "daemon"), Log::Actions);
    }

    /// Both logs rotate at their own cap, independently: a poll storm turns
    /// `polls.jsonl` over and leaves every action line in `calls.jsonl`.
    #[test]
    fn polls_and_actions_rotate_at_their_own_caps() {
        let dir = std::env::temp_dir().join(format!("wsp-calls-two-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let pad = "x".repeat(200);
        let size = |f: &str| std::fs::metadata(dir.join(f)).map(|m| m.len()).unwrap_or(0);
        append_to(&dir, Log::Actions, &json!({"k":"call","m":"action-0","pad":pad}), 4096);
        let mut line_len = 0;
        for i in 0..120 {
            let line = json!({"k":"call","m":format!("poll-{i:04}"),"pad":pad});
            line_len = line.to_string().len() as u64 + 1;
            append_to(&dir, Log::Polls, &line, 4096);
            assert!(size(POLL_FILE) < 4096 + line_len);
            assert!(size(POLL_FILE) + size(POLL_OLD) <= 2 * (4096 + line_len));
        }
        assert!(size(POLL_OLD) > 0, "polls.jsonl rotated to polls.1.jsonl");
        assert!(!dir.join(OLD).exists(), "a poll storm rotates nothing in calls.jsonl");
        let actions = read_log(&dir, Log::Actions);
        assert_eq!(actions.len(), 1);
        assert_eq!(actions[0]["m"], "action-0", "the action is still there");
        let polls = read_log(&dir, Log::Polls);
        assert!(polls.iter().any(|v| v["m"] == "poll-0119"));
        assert!(!polls.iter().any(|v| v["m"] == "poll-0000"), "the oldest polls are gone");
        // And the actions rotate on their own cap.
        for i in 0..60 {
            append_to(&dir, Log::Actions, &json!({"k":"call","m":format!("act-{i}"),"pad":pad}), 4096);
        }
        assert!(size(OLD) > 0 && size(FILE) < 4096 + line_len);
        let cov = Coverage::of_log(&read_log(&dir, Log::Polls), &dir, Log::Polls);
        assert_eq!(cov.live_bytes, size(POLL_FILE));
        assert!(cov.line(3600, 0).contains("polls.jsonl"), "{}", cov.line(3600, 0));
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// A poll line is about this long, which is what the per-poll-line choice
    /// in `docs/calls.md` is worked out from.
    #[test]
    fn a_poll_line_is_a_few_hundred_bytes() {
        let mut k = Kids::default();
        k.0.insert("ps".into(), (2, 300.0));
        k.0.insert("git".into(), (4, 40.0));
        let v = call_line(0, "worklist", "show", &["all".to_string(), "json".to_string()], None, "none", "compound-host", Duration::from_millis(900), (400_000, 60_000), (300_000, 80_000), &k, Some(0));
        let n = v.to_string().len();
        assert!((250..700).contains(&n), "{n} bytes: {v}");
    }

    /// The bound Ed asked for is in the constant, not just in the test's cap.
    #[test]
    fn the_real_cap_is_twenty_mebibytes_so_both_files_stay_under_forty() {
        assert_eq!(CAP, 20 * 1024 * 1024);
        assert_eq!(POLL_CAP, 20 * 1024 * 1024);
    }

    /// The file's coverage is read off its records, not off the window asked
    /// for, and a window longer than the file says so.
    #[test]
    fn coverage_says_when_the_file_is_shorter_than_the_window() {
        let dir = std::env::temp_dir().join(format!("wsp-calls-cov-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let now = crate::util::epoch_secs();
        let line = |ago: i64| json!({"k":"call","t":crate::util::iso_at(now - ago),"verb":"x"});
        append(&dir, &line(3 * 3600 + 600), CAP);
        append(&dir, &json!({"k":"tick","t":crate::util::iso_at(now - 60)}), CAP);
        let lines = read_all(&dir);
        let cov = Coverage::of(&lines, &dir);
        assert_eq!(cov.span(), 3 * 3600 + 540);
        let two_days = 2 * 86_400;
        assert!(cov.short(now - two_days));
        let said = cov.line(two_days, now - two_days);
        assert!(said.contains("calls.jsonl covers 3h"), "{said}");
        assert!(said.contains("SHORT: asked for 2d"), "{said}");
        assert!(!cov.short(now - 3 * 3600), "a window the file spans is not short");
        assert!(!cov.line(3 * 3600, now - 3 * 3600).contains("SHORT"));
        assert!(cov.short(now - 4 * 3600), "a 4h window over a 3h10m file is short");
        let j = cov.json(now - two_days);
        assert_eq!(j["short"], true);
        assert_eq!(j["span_s"], 3 * 3600 + 540);
        assert!(j["first"].is_string() && j["last"].is_string() && j["live_bytes"].as_u64().unwrap() > 0);
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// Sorted by CPU, with each row's count and percentiles; the share is of all
    /// the CPU in the window.
    #[test]
    fn the_report_ranks_by_total_cpu() {
        let lines = vec![
            call("wip", "", &["json"], "none", "compound-host", 12000.0, 9000.0, json!({"ps":{"n":2,"ms":300.0},"lsof":{"n":1,"ms":900.0}})),
            call("wip", "", &["json"], "none", "compound-host", 8000.0, 6000.0, json!({"ps":{"n":2,"ms":200.0}})),
            call("tell", "", &[], "member", "claude", 90.0, 20.0, json!({})),
            json!({"k":"tick","t":"2026-11-07T10:00:00Z"}),
        ];
        let (rows, total) = summarize(&lines, 0, By::Verb);
        assert_eq!(total, 15020.0, "ticks are not calls");
        assert_eq!(rows[0].name, "wip --json");
        assert_eq!(rows[0].count, 2);
        assert_eq!(rows[0].p(0.5), 8000.0);
        assert_eq!(rows[0].p(0.95), 12000.0);
        assert_eq!(rows[1].name, "tell");
        let (by_caller, _) = summarize(&lines, 0, By::Caller);
        assert_eq!(by_caller[0].name, "none via compound-host");
        let (by_child, _) = summarize(&lines, 0, By::Child);
        assert_eq!(by_child[0].name, "lsof", "ranked by total wall: {by_child:?}");
        assert_eq!(by_child.iter().find(|r| r.name == "ps").unwrap().count, 4);
    }

    #[test]
    fn a_call_older_than_the_window_is_left_out() {
        let lines = vec![call("wip", "", &[], "none", "", 1.0, 1.0, json!({}))];
        let later = crate::util::epoch_of("2026-11-07T11:00:00Z");
        assert!(summarize(&lines, later, By::Verb).0.is_empty());
    }

    /// The tick line names each phase, adds a phase named twice, and carries the
    /// children of that tick.
    #[test]
    fn a_tick_line_has_ms_per_phase() {
        let mut k = Kids::default();
        k.0.insert("git".into(), (4, 12.0));
        let v = tick_line(0, Duration::from_millis(900), (10_000, 0), (0, 0), &[("sync".into(), 700.5), ("cycle".into(), 100.0)], &k);
        assert_eq!(v["k"], "tick");
        assert_eq!(v["phases"]["sync"], 700.5);
        assert_eq!(v["phases"]["cycle"], 100.0);
        assert_eq!(v["children"]["git"]["n"], 4);
    }

    #[test]
    fn phases_add_when_a_name_comes_round_again() {
        let mut t = Tick { at: 0, started: Instant::now(), cpu0: (0, 0), kids0: (0, 0), phase: None, phases: vec![] };
        t.phase = Some(("a".into(), Instant::now() - Duration::from_millis(30)));
        close(&mut t);
        t.phase = Some(("a".into(), Instant::now() - Duration::from_millis(30)));
        close(&mut t);
        assert_eq!(t.phases.len(), 1);
        assert!(t.phases[0].1 >= 59.0);
    }

    /// The whole of what an exit does — role, parent name, both `getrusage`s,
    /// the line, the append — costs under a millisecond. Measured in the CPU
    /// this process spends, not the wall clock: a loaded machine (four agents
    /// build here by design) stretches the wall time of a batch by however long
    /// the process waited for a core, and that is not what an exit costs. Still
    /// the best of several batches. Run with `--release` for the number a
    /// shipped binary has; the bound holds in a debug build too.
    #[test]
    fn the_record_an_exit_writes_costs_under_a_millisecond() {
        let dir = std::env::temp_dir().join(format!("wsp-calls-cost-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        // A governors record the size of the real one, not naming this seat.
        std::fs::write(dir.join("governors.json"), format!("{{\"x\":{{\"pad\":\"{}\"}}}}", "y".repeat(30_000))).unwrap();
        let mut kids = Kids::default();
        kids.0.insert("ps".into(), (3, 41.0));
        kids.0.insert("lsof".into(), (1, 160.0));
        let spent = || {
            let (u, s) = own_cpu();
            Duration::from_micros(u + s)
        };
        let mut best = Duration::MAX;
        for _ in 0..5 {
            let t = spent();
            for _ in 0..50 {
                let r = role(Some("cpd-1"), "wip", Some(&dir));
                let parent = process_name(unsafe { getppid() });
                let line = call_line(
                    1_000_000, "wip", "", &["json".to_string()], Some("cpd-1"), r, &parent,
                    Duration::from_millis(5), own_cpu(), children_cpu(), &kids, Some(0),
                );
                append(&dir, &line, CAP);
            }
            best = best.min((spent() - t) / 50);
        }
        let _ = std::fs::remove_dir_all(&dir);
        eprintln!("a call record costs {best:?}");
        assert!(best < Duration::from_millis(1), "a record cost {best:?} per call");
    }

    /// The parent's name comes from the kernel, not from a child process.
    #[test]
    fn the_parent_has_a_name() {
        assert!(!process_name(unsafe { getppid() }).is_empty());
    }

    #[test]
    fn this_process_has_spent_some_cpu() {
        let mut x = 0u64;
        for i in 0..5_000_000u64 {
            x = x.wrapping_add(i * i);
        }
        std::hint::black_box(x);
        let (u, s) = own_cpu();
        assert!(u + s > 0);
    }
}
