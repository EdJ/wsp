//! What a wsp call costs, written down (`wsp-481`).
//!
//! Ed, 2026-11-07, after `wip --json` was measured by hand at 12.6 s: "add a
//! feature to wsp to record the times of its calls, so we can start to optimize
//! those". Nothing recorded it before; wsp-480 was found with `/usr/bin/time`
//! and `sample` against a binary with no symbols.
//!
//! **Two kinds of line, one file** — `$WSP_STATE/calls.jsonl`, appended with
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
//! of it off. The file rotates to `calls.1.jsonl` past [`CAP`].

use std::collections::BTreeMap;
use std::ffi::OsStr;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicI32, Ordering};
use std::sync::Mutex;
use std::time::{Duration, Instant};

use serde_json::{json, Value};

/// Rotate to `calls.1.jsonl` when the file is past this.
pub const CAP: u64 = 20 * 1024 * 1024;
pub const FILE: &str = "calls.jsonl";
pub const OLD: &str = "calls.1.jsonl";

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
    let mut s = line.to_string();
    s.push('\n');
    let path = dir.join(FILE);
    if std::fs::metadata(&path).is_ok_and(|m| m.len() >= cap) {
        let _ = std::fs::rename(&path, dir.join(OLD));
    }
    let f = std::fs::OpenOptions::new().create(true).append(true).open(&path);
    if let Ok(mut f) = f {
        let _ = f.write_all(s.as_bytes());
    }
}

fn write(line: Value) {
    if off() {
        return;
    }
    if let Some(dir) = state_dir() {
        if dir.is_dir() {
            append(&dir, &line, CAP);
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
    write(line);
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
    write(line);
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
    let mut out = Vec::new();
    for name in [OLD, FILE] {
        if let Ok(text) = std::fs::read_to_string(dir.join(name)) {
            out.extend(text.lines().filter_map(|l| serde_json::from_str::<Value>(l).ok()));
        }
    }
    out
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

    /// The file grows by one line per append and rotates once past the cap, so
    /// it never exceeds roughly the cap plus one line.
    #[test]
    fn the_file_rotates_at_its_cap() {
        let dir = std::env::temp_dir().join(format!("wsp-calls-rot-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let line = json!({"k":"call","pad":"x".repeat(200)});
        for _ in 0..100 {
            append(&dir, &line, 4096);
        }
        let live = std::fs::metadata(dir.join(FILE)).unwrap().len();
        assert!(live < 4096 + 300, "the live file stays under cap + one line: {live}");
        assert!(dir.join(OLD).exists(), "the full file moved to calls.1.jsonl");
        assert!(read_all(&dir).iter().all(|v| v["k"] == "call"));
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
    /// the line, the append — costs under a millisecond. The best of several
    /// batches, because a loaded machine (four agents build here by design)
    /// slows a batch and never speeds one. Run with `--release` for the
    /// number a shipped binary has; the bound holds in a debug build too.
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
        let mut best = Duration::MAX;
        for _ in 0..5 {
            let t = Instant::now();
            for _ in 0..50 {
                let r = role(Some("cpd-1"), "wip", Some(&dir));
                let parent = process_name(unsafe { getppid() });
                let line = call_line(
                    1_000_000, "wip", "", &["json".to_string()], Some("cpd-1"), r, &parent,
                    Duration::from_millis(5), own_cpu(), children_cpu(), &kids, Some(0),
                );
                append(&dir, &line, CAP);
            }
            best = best.min(t.elapsed() / 50);
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
