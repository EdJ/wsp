//! Builds on a shared machine: `wsp-331`.
//!
//! Ed, 2026-11-01 23:33: load average 48 on 8 cores and the machine unusable.
//! One agent was running two cargo builds at once in one tree, one of them
//! compiling gpui, and each fanned out to every core. Other agents and Ed's own
//! debug window were competing with it.
//!
//! # Two things that look alike and are not
//!
//! **A throttle** is a courtesy to the person at the machine: builds run niced,
//! under a job cap, and wait their turn for a machine-wide slot. It is a
//! setting with three modes, `off`, `on` and `auto` (the default), and `auto`
//! holds it only while the person is there, read from the HID idle time
//! `ioreg` reports. Heavy use is fine on a machine nobody is using.
//!
//! **One build per tree** is correctness, not courtesy. Two cargos in one tree
//! segfaulted the linker, and it reads as a compile error in whatever the agent
//! was changing. It is held in every mode, including `off`.
//!
//! # How it reaches a build
//!
//! Every seat wsp spawns gets a directory of shims first on its `PATH`
//! ([`seat_env`]), one script per tool that execs `wsp throttle run <tool>`.
//! The decision is made **at each build, not at spawn**, and that is the whole
//! shape of this module: `auto` flips while an agent is running, and a niceness
//! can be raised but never lowered without root, so an agent niced at spawn
//! would stay niced after the person left, which is exactly what `auto`
//! promises to lift. A build is short-lived, so it is the right unit to nice.
//! The agent itself waits on the network and burns nothing worth niceing.
//!
//! [`run_in`] resolves the real tool past the shims, holds the tree, and, when
//! the throttle is in force, takes a slot, sets the job cap and runs the build
//! niced. A nested invocation (a build script calling cargo, a test running a
//! fixture build) finds [`HELD`] in its environment and passes straight
//! through, because the build above it already holds everything and waiting for
//! it would deadlock.
//!
//! # Holds are files, and a hold belongs to a process
//!
//! A slot and a tree are each a file under `builds/` in the state directory,
//! created `O_EXCL`, naming the `wsp throttle run` process that owns it. There
//! is no release to forget: a holder that dies (SIGKILL, a terminal closed, an
//! agent's tool call timing out) is found dead by the next waiter, which takes
//! the file over under a short-lived mutex so two waiters cannot both inherit
//! it. Only the wait for a slot is bounded by nothing, deliberately: a build
//! that is queued is not failing, and the message says so and says who has the
//! slots.
//!
//! Only subcommands that compile and finish take the tree and a slot
//! ([`heavy`]). `cargo run` does not: it may stay up for hours, and a hold on
//! the tree that outlasted the compile would block every build after it. Cargo's
//! own lock already serialises its compile phase.
//!
//! # Seeing it
//!
//! `wsp throttle` prints the mode, whether it is in force this minute, the
//! load, and which builds are running and for whom. The load warning (the
//! 1-minute and the 5-minute average both over 1.5 times the core count, so
//! it has lasted) shows in every mode, on `wsp wip`, `wsp watch --status` and
//! `wsp doctor`, and the daemon's tick reaps dead holds and logs the warning
//! when it starts and clears.

use std::collections::BTreeMap;
use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use serde_json::{json, Value};

use crate::store::Store;
use crate::util;
use crate::Args;

/// Set in the environment of a build wsp is holding, so a build nested in it
/// passes through. `WSP_`-prefixed so a spawned seat sheds it
/// ([`crate::cmd_spawn`]'s `unowned`) and is never born inside someone's hold.
pub(crate) const HELD: &str = "WSP_BUILD_HELD";

/// How much a throttled build is niced by.
const NICE: i32 = 12;

/// Minutes without a keypress or mouse move after which the person has left.
const AWAY_MINUTES: u64 = 5;

/// Load over this many times the core count, in both the 1- and 5-minute
/// average, is a machine nobody can work on.
const LOAD_FACTOR: f64 = 1.5;

/// Tools that get a shim when the settings declare none, and the variable that
/// caps each one's parallelism.
fn default_tools() -> BTreeMap<String, String> {
    BTreeMap::from([("cargo".to_string(), "CARGO_BUILD_JOBS".to_string())])
}

/// What `cargo` subcommands compile something and then finish. Aliases
/// included. `run` is [`Weight::Runs`]; anything else passes through.
const HEAVY: &[&str] = &[
    "build", "b", "check", "c", "test", "t", "clippy", "bench", "doc", "d", "install", "rustc", "fix", "nextest",
    "llvm-cov", "miri",
];

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Mode {
    Off,
    On,
    Auto,
}

impl Mode {
    pub(crate) fn word(self) -> &'static str {
        match self {
            Mode::Off => "off",
            Mode::On => "on",
            Mode::Auto => "auto",
        }
    }

    fn parse(s: &str) -> Option<Mode> {
        match s {
            "off" => Some(Mode::Off),
            "on" => Some(Mode::On),
            "auto" => Some(Mode::Auto),
            _ => None,
        }
    }
}

/// The machine's choice, in `throttle.json` under the state directory: it is
/// this machine's and not the store's, so it is not committed or synced.
#[derive(Clone, Debug)]
pub(crate) struct Settings {
    pub mode: Mode,
    /// Heavy builds at once across the fleet. `None` is a quarter of the cores.
    pub slots: Option<usize>,
    /// Parallel jobs in one build. `None` is the cores, less one, shared out
    /// across the slots.
    pub jobs: Option<usize>,
    pub away_minutes: u64,
    /// Tool name to the variable that caps its parallelism. Edited by hand in
    /// the file; the default is cargo.
    pub tools: BTreeMap<String, String>,
}

impl Default for Settings {
    fn default() -> Settings {
        Settings { mode: Mode::Auto, slots: None, jobs: None, away_minutes: AWAY_MINUTES, tools: default_tools() }
    }
}

fn settings_path(state: &Path) -> PathBuf {
    state.join("throttle.json")
}

impl Settings {
    pub(crate) fn load(state: &Path) -> Settings {
        let mut s = Settings::default();
        let Some(v) = fs::read_to_string(settings_path(state)).ok().and_then(|t| serde_json::from_str::<Value>(&t).ok())
        else {
            return s;
        };
        if let Some(m) = v.get("mode").and_then(Value::as_str).and_then(Mode::parse) {
            s.mode = m;
        }
        s.slots = v.get("slots").and_then(Value::as_u64).map(|n| n.max(1) as usize);
        s.jobs = v.get("jobs").and_then(Value::as_u64).map(|n| n.max(1) as usize);
        if let Some(n) = v.get("away_minutes").and_then(Value::as_u64) {
            s.away_minutes = n;
        }
        if let Some(t) = v.get("tools").and_then(Value::as_object) {
            let tools: BTreeMap<String, String> =
                t.iter().filter_map(|(k, v)| v.as_str().map(|v| (k.clone(), v.to_string()))).collect();
            if !tools.is_empty() {
                s.tools = tools;
            }
        }
        s
    }

    pub(crate) fn save(&self, state: &Path) -> std::io::Result<()> {
        let _ = fs::create_dir_all(state);
        let v = json!({
            "mode": self.mode.word(),
            "slots": self.slots,
            "jobs": self.jobs,
            "away_minutes": self.away_minutes,
            "tools": self.tools,
        });
        crate::store::write_atomic(&settings_path(state), &serde_json::to_string_pretty(&v).unwrap_or_default())
    }

    pub(crate) fn slot_count(&self, cores: usize) -> usize {
        self.slots.unwrap_or((cores / 4).max(1))
    }

    pub(crate) fn job_cap(&self, cores: usize) -> usize {
        self.jobs.unwrap_or_else(|| (cores.saturating_sub(1) / self.slot_count(cores)).max(1))
    }
}

/// Whether the throttle holds right now. When `auto` cannot read the idle time
/// it holds: wrongly throttling an idle machine costs minutes, and wrongly
/// not throttling one with a person at it costs the person.
pub(crate) fn in_force(mode: Mode, idle_secs: Option<u64>, away_minutes: u64) -> bool {
    match mode {
        Mode::Off => false,
        Mode::On => true,
        Mode::Auto => idle_secs.is_none_or(|i| i < away_minutes * 60),
    }
}

/// `"HIDIdleTime" = 4294967296` in `ioreg -c IOHIDSystem` output, nanoseconds.
pub(crate) fn parse_hid_idle(ioreg: &str) -> Option<u64> {
    let line = ioreg.lines().find(|l| l.contains("\"HIDIdleTime\""))?;
    let ns: u64 = line.split('=').nth(1)?.trim().parse().ok()?;
    Some(ns / 1_000_000_000)
}

fn hid_idle_secs() -> Option<u64> {
    let out = Command::new("ioreg").args(["-c", "IOHIDSystem", "-d", "4"]).stderr(Stdio::null()).output().ok()?;
    out.status.success().then(|| parse_hid_idle(&String::from_utf8_lossy(&out.stdout))).flatten()
}

/// Three load averages out of `/proc/loadavg` (`0.5 0.4 0.3 1/200 99`) or
/// `sysctl -n vm.loadavg` (`{ 0.5 0.4 0.3 }`).
pub(crate) fn parse_loadavg(s: &str) -> Option<(f64, f64, f64)> {
    let mut n = s.split(|c: char| c.is_whitespace() || c == '{' || c == '}').filter(|w| !w.is_empty());
    Some((n.next()?.parse().ok()?, n.next()?.parse().ok()?, n.next()?.parse().ok()?))
}

fn load_average() -> Option<(f64, f64, f64)> {
    if let Some(l) = fs::read_to_string("/proc/loadavg").ok().and_then(|s| parse_loadavg(&s)) {
        return Some(l);
    }
    let out = Command::new("sysctl").args(["-n", "vm.loadavg"]).stderr(Stdio::null()).output().ok()?;
    parse_loadavg(&String::from_utf8_lossy(&out.stdout))
}

pub(crate) fn load_high(one: f64, five: f64, cores: usize) -> bool {
    one.min(five) > LOAD_FACTOR * cores as f64
}

/// How many times the readiness wait of a spawn is stretched at this load
/// (`wsp-335`): once per core's worth of load queued, from 1 to 4. A
/// `compound-956` agent never became ready inside sixty seconds at load about
/// 60 on 8 cores; the same agent is up in twenty on a quiet machine, so the
/// wait grows with the load rather than for everyone. The cap is four minutes:
/// past that the start is better ended and tried again on the backoff.
pub(crate) fn readiness_scale(one: f64, cores: usize) -> u32 {
    ((one / cores.max(1) as f64).ceil() as u32).clamp(1, 4)
}

/// [`readiness_scale`] for the machine as it is now. 1 when the load cannot be
/// read, and under test, where the clock is a dial and a load reading is noise.
pub(crate) fn readiness_scale_now() -> u32 {
    if cfg!(test) {
        return 1;
    }
    load_average().map_or(1, |(one, _, _)| readiness_scale(one, cores()))
}

fn cores() -> usize {
    std::thread::available_parallelism().map(|n| n.get()).unwrap_or(1)
}

/// The `cargo` subcommand: the first word that is not a `+toolchain` or a flag.
fn subcommand(args: &[String]) -> Option<&str> {
    args.iter().map(String::as_str).find(|a| !a.starts_with('+') && !a.starts_with('-'))
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Weight {
    /// Compiles and finishes: holds the tree and takes a slot.
    Heavy,
    /// Compiles and may then stay up (`cargo run`): niced and capped like a
    /// heavy build, but holds nothing, so the compile it contains is not the
    /// unthrottled one and a long run does not hold the tree.
    Runs,
    Light,
}

pub(crate) fn weight(tool: &str, args: &[String]) -> Weight {
    if tool != "cargo" {
        return Weight::Heavy;
    }
    match subcommand(args) {
        Some(s) if HEAVY.contains(&s) => Weight::Heavy,
        Some("run" | "r") => Weight::Runs,
        _ => Weight::Light,
    }
}

fn alive(pid: u32) -> bool {
    pid != 0
        && Command::new("kill").args(["-0", &pid.to_string()]).stderr(Stdio::null()).status().is_ok_and(|s| s.success())
}

fn fnv(s: &str) -> String {
    let mut h: u64 = 0xcbf2_9ce4_8422_2325;
    for b in s.bytes() {
        h = (h ^ b as u64).wrapping_mul(0x0100_0000_01b3);
    }
    format!("{h:016x}")
}

/// The tree a build is in: the nearest ancestor with a `.git`, else the
/// directory itself. A worktree has a `.git` file, so each agent's tree is its
/// own.
pub(crate) fn tree_of(cwd: &Path) -> PathBuf {
    let start = fs::canonicalize(cwd).unwrap_or_else(|_| cwd.to_path_buf());
    start.ancestors().find(|d| d.join(".git").exists()).unwrap_or(&start).to_path_buf()
}

// ---- holds ------------------------------------------------------------------

fn builds_dir(state: &Path) -> PathBuf {
    state.join("builds")
}

fn record(task: &str, tool: &str, args: &[String], tree: &Path) -> Value {
    let cmd = std::iter::once(tool).chain(args.iter().map(String::as_str)).collect::<Vec<_>>().join(" ");
    json!({
        "pid": std::process::id(),
        "task": task,
        "cmd": cmd.chars().take(80).collect::<String>(),
        "tree": tree.display().to_string(),
        "since": util::epoch_secs(),
    })
}

fn holder_pid(v: &Value) -> u32 {
    v.get("pid").and_then(Value::as_u64).unwrap_or(0) as u32
}

/// A hold taken. Dropping it releases the file, if it is still ours.
pub(crate) struct Hold {
    path: PathBuf,
}

impl Drop for Hold {
    fn drop(&mut self) {
        let mine = fs::read_to_string(&self.path)
            .ok()
            .and_then(|t| serde_json::from_str::<Value>(&t).ok())
            .is_some_and(|v| holder_pid(&v) == std::process::id());
        if mine {
            let _ = fs::remove_file(&self.path);
        }
    }
}

enum Taken {
    Yours(Hold),
    HeldBy(Value),
}

/// Try for the hold at `path`. A file whose owner is dead, or that never got
/// its contents and is old, is taken over under a mutex.
fn take(path: &Path, rec: &Value, alive: &dyn Fn(u32) -> bool) -> Taken {
    if let Some(dir) = path.parent() {
        let _ = fs::create_dir_all(dir);
    }
    let text = rec.to_string();
    match fs::OpenOptions::new().write(true).create_new(true).open(path) {
        Ok(mut f) => {
            let _ = f.write_all(text.as_bytes());
            return Taken::Yours(Hold { path: path.to_path_buf() });
        }
        Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => {}
        // An unwritable state directory is not a reason to stop a build.
        Err(_) => return Taken::Yours(Hold { path: PathBuf::new() }),
    }
    let current = || fs::read_to_string(path).ok().and_then(|t| serde_json::from_str::<Value>(&t).ok());
    let age = || {
        fs::metadata(path).and_then(|m| m.modified()).ok().and_then(|t| t.elapsed().ok()).unwrap_or_default()
    };
    let stale = |v: &Option<Value>| match v {
        Some(v) => !alive(holder_pid(v)),
        None => age() > Duration::from_secs(5),
    };
    let seen = current();
    if !stale(&seen) {
        return Taken::HeldBy(seen.unwrap_or(Value::Null));
    }
    let mutex = path.with_extension("reclaim");
    if let Ok(m) = fs::metadata(&mutex) {
        if m.modified().ok().and_then(|t| t.elapsed().ok()).is_some_and(|e| e > Duration::from_secs(10)) {
            let _ = fs::remove_file(&mutex);
        }
    }
    if fs::OpenOptions::new().write(true).create_new(true).open(&mutex).is_err() {
        return Taken::HeldBy(seen.unwrap_or(Value::Null));
    }
    let again = current();
    let out = match stale(&again) {
        true => match fs::write(path, &text) {
            Ok(()) => Taken::Yours(Hold { path: path.to_path_buf() }),
            Err(_) => Taken::HeldBy(Value::Null),
        },
        false => Taken::HeldBy(again.unwrap_or(Value::Null)),
    };
    let _ = fs::remove_file(&mutex);
    out
}

fn tree_path(state: &Path, tree: &Path) -> PathBuf {
    builds_dir(state).join(format!("tree-{}", fnv(&tree.display().to_string())))
}

fn slot_path(state: &Path, n: usize) -> PathBuf {
    builds_dir(state).join(format!("slot-{n}"))
}

/// What a build in the line is told while it waits.
fn who(v: &Value) -> String {
    let task = v.get("task").and_then(Value::as_str).filter(|t| !t.is_empty()).unwrap_or("someone");
    let cmd = v.get("cmd").and_then(Value::as_str).unwrap_or("a build");
    match v.get("since").and_then(Value::as_i64) {
        Some(s) => format!("{task} `{cmd}` for {}", util::duration_human(util::epoch_secs() - s)),
        None => format!("{task} `{cmd}`"),
    }
}

// ---- running a build --------------------------------------------------------

/// Everything a build decision reads, so it can be made against a fake.
pub(crate) struct Ctx<'a> {
    pub state: &'a Path,
    pub cwd: PathBuf,
    pub path_var: String,
    pub task: String,
    pub cores: usize,
    pub idle: &'a dyn Fn() -> Option<u64>,
    pub alive: &'a dyn Fn(u32) -> bool,
    pub say: &'a dyn Fn(&str),
    pub poll: Duration,
    /// Set for a build already inside a hold.
    pub held: bool,
    /// The variable already in the build's environment, by name.
    pub env: &'a dyn Fn(&str) -> Option<String>,
}

fn shim_dir(state: &Path) -> PathBuf {
    state.join("shims")
}

/// The line every shim carries; a file that has it is wsp's, whichever state
/// directory it lives in.
const SHIM_MARK: &str = "# wsp's build throttle (wsp-331)";

/// Is `p` a shim wsp wrote? A temp `WSP_STATE` has its own shims dir, but the
/// `PATH` it inherits can still carry the real seat's; resolving to that one
/// runs `wsp throttle run` again, forever (wsp-367).
fn is_shim(p: &Path) -> bool {
    use std::io::Read;
    let mut head = [0u8; 160];
    let n = fs::File::open(p).and_then(|mut f| f.read(&mut head)).unwrap_or(0);
    String::from_utf8_lossy(&head[..n]).starts_with("#!/bin/sh\n") && String::from_utf8_lossy(&head[..n]).contains(SHIM_MARK)
}

/// The first `tool` on `path` that is not one of ours.
pub(crate) fn real_tool(tool: &str, path: &str, shims: &Path) -> Option<PathBuf> {
    use std::os::unix::fs::PermissionsExt;
    path.split(':')
        .filter(|d| !d.is_empty() && Path::new(d) != shims)
        .map(|d| Path::new(d).join(tool))
        .find(|p| fs::metadata(p).is_ok_and(|m| m.is_file() && m.permissions().mode() & 0o111 != 0) && !is_shim(p))
}

/// Whether `cargo test` is the whole suite: no filter, no target or package
/// narrowing it. Any bare word after the subcommand is a filter (or a flag's
/// value, which errs toward calling the run narrow, never the other way).
pub(crate) fn is_full_suite(tool: &str, args: &[String]) -> bool {
    if tool != "cargo" || !matches!(subcommand(args), Some("test" | "t")) {
        return false;
    }
    const NARROW: &[&str] = &["--lib", "--bins", "--bin", "--test", "--tests", "--doc", "--example", "--examples", "--bench", "--benches", "-p", "--package"];
    let at = args.iter().position(|a| !a.starts_with('+') && !a.starts_with('-')).unwrap_or(0);
    let rest = &args[at + 1..];
    !rest.iter().any(|a| !a.starts_with('-') || NARROW.contains(&a.split('=').next().unwrap_or("")))
}

fn full_suite_dir(state: &Path) -> PathBuf {
    state.join("full-suite")
}

/// Note a full-suite `cargo test` started in a row's seat (`wsp-389`): the
/// suite is the barrier's, once per group. A check or the barrier's own run has
/// `WSP_*` stripped from its environment, so a task here is a member's or a
/// verifier's seat. One file per row, kept (the effort report sums `runs`),
/// with the runs by seat: `wsp wip` flags a seat that is in it, so a row
/// reopened into a new seat is not flagged for the last one's run.
fn note_full_suite(ctx: &Ctx) {
    let path = full_suite_dir(ctx.state).join(format!("{}.json", ctx.task));
    let old = fs::read_to_string(&path).ok().and_then(|t| serde_json::from_str::<Value>(&t).ok()).unwrap_or(Value::Null);
    let runs = old.get("runs").and_then(Value::as_u64).unwrap_or(0) + 1;
    let mut seats = old.get("seats").and_then(Value::as_object).cloned().unwrap_or_default();
    if let Some(seat) = (ctx.env)(crate::place::SEAT_ENV) {
        let n = seats.get(&seat).and_then(Value::as_u64).unwrap_or(0) + 1;
        seats.insert(seat, json!(n));
    }
    let _ = fs::create_dir_all(full_suite_dir(ctx.state));
    let rec = json!({
        "task": ctx.task, "runs": runs, "seats": seats, "since": util::epoch_secs(),
        "tree": tree_of(&ctx.cwd).display().to_string(),
    });
    let _ = crate::store::write_atomic(&path, &rec.to_string());
    (ctx.say)(
        "wsp: that is the full suite, and in a member's or verifier's seat it is noted (`wsp wip` says so). Run the tests for what you changed (`cargo test <module>::`); the full suite runs once per group, at the barrier.",
    );
}

fn full_suite_record(state: &Path, task: &str) -> Value {
    fs::read_to_string(full_suite_dir(state).join(format!("{task}.json")))
        .ok()
        .and_then(|t| serde_json::from_str::<Value>(&t).ok())
        .unwrap_or(Value::Null)
}

/// How many full suites a row's seats have started, ever.
pub(crate) fn full_suite_runs(state: &Path, task: &str) -> u64 {
    full_suite_record(state, task).get("runs").and_then(Value::as_u64).unwrap_or(0)
}

/// The seats that have started a full suite.
pub(crate) fn full_suite_seats(state: &Path) -> std::collections::BTreeSet<String> {
    let Ok(rd) = fs::read_dir(full_suite_dir(state)) else { return Default::default() };
    rd.flatten()
        .filter_map(|e| fs::read_to_string(e.path()).ok())
        .filter_map(|t| serde_json::from_str::<Value>(&t).ok())
        .flat_map(|v| v.get("seats").and_then(Value::as_object).map(|o| o.keys().cloned().collect::<Vec<_>>()).unwrap_or_default())
        .collect()
}

pub(crate) fn run_in(ctx: &Ctx, tool: &str, args: &[String]) -> i32 {
    let settings = Settings::load(ctx.state);
    let shims = shim_dir(ctx.state);
    let Some(real) = real_tool(tool, &ctx.path_var, &shims) else {
        eprintln!("wsp: no `{tool}` on PATH past wsp's own shim");
        return 127;
    };
    if !ctx.held && !ctx.task.is_empty() && is_full_suite(tool, args) {
        note_full_suite(ctx);
    }
    let mut cmd = Command::new(&real);
    cmd.args(args);
    let weight = weight(tool, args);
    if ctx.held || weight == Weight::Light {
        return finish(cmd, None);
    }
    let force = || {
        let idle = match settings.mode {
            Mode::Auto => (ctx.idle)(),
            _ => None,
        };
        in_force(settings.mode, idle, settings.away_minutes)
    };
    let mut _holds = Vec::new();
    if weight == Weight::Heavy {
        let tree = tree_of(&ctx.cwd);
        let rec = record(&ctx.task, tool, args, &tree);
        // The tree first and the slot second: a build waiting on its own tree
        // must not be sitting on a slot somebody else could use.
        if tool == "cargo" {
            let mut grab = || take(&tree_path(ctx.state, &tree), &rec, ctx.alive);
            _holds.push(wait_for(ctx, "its tree", &mut grab, None));
        }
        if force() {
            let n = settings.slot_count(ctx.cores);
            let mut grab = || {
                let mut seen = Value::Null;
                for i in 0..n {
                    match take(&slot_path(ctx.state, i), &rec, ctx.alive) {
                        Taken::Yours(h) => return Taken::Yours(h),
                        Taken::HeldBy(v) => seen = v,
                    }
                }
                Taken::HeldBy(seen)
            };
            _holds.push(wait_for(ctx, "a build slot", &mut grab, Some(&|| !force())));
        }
        cmd.env(HELD, tree.display().to_string());
    }
    let mut niced = None;
    if force() {
        niced = Some(NICE);
        if let Some(var) = settings.tools.get(tool) {
            let wanted = settings.job_cap(ctx.cores);
            let theirs = (ctx.env)(var).and_then(|v| v.parse::<usize>().ok());
            cmd.env(var, theirs.map_or(wanted, |t| t.min(wanted)).to_string());
        }
    }
    finish(cmd, niced)
}

/// Keep trying until `attempt` is ours, saying who has it at the start and
/// each minute. `lifted` ends the wait for nothing when there is no longer
/// anything to wait for (`auto`, with the person gone), looked at every ten
/// polls rather than each because it may cost a process.
fn wait_for(ctx: &Ctx, what: &str, attempt: &mut dyn FnMut() -> Taken, lifted: Option<&dyn Fn() -> bool>) -> Hold {
    let mut waited = Duration::ZERO;
    let mut next_say = Duration::ZERO;
    let mut polls = 0u32;
    loop {
        let held_by = match attempt() {
            Taken::Yours(h) => return h,
            Taken::HeldBy(v) => v,
        };
        if waited >= next_say {
            let by = match held_by {
                Value::Null => "another build".to_string(),
                v => who(&v),
            };
            (ctx.say)(&format!(
                "wsp: waiting for {what} — held by {by}. Queued, not failed; this is wsp's build throttle (`wsp throttle` shows it, `wsp throttle off` lifts the slots, not the one-build-per-tree hold)"
            ));
            next_say = waited + Duration::from_secs(60);
        }
        std::thread::sleep(ctx.poll);
        waited += ctx.poll;
        polls += 1;
        if polls % 10 == 0 && lifted.is_some_and(|l| l()) {
            return Hold { path: PathBuf::new() };
        }
    }
}

fn finish(mut cmd: Command, nice: Option<i32>) -> i32 {
    use std::os::unix::process::{CommandExt, ExitStatusExt};
    extern "C" {
        fn setpriority(which: i32, who: u32, prio: i32) -> i32;
        fn signal(sig: i32, handler: usize) -> usize;
    }
    if let Some(n) = nice {
        // SAFETY: setpriority is async-signal-safe and touches nothing of ours.
        unsafe {
            cmd.pre_exec(move || {
                setpriority(0, 0, n);
                Ok(())
            });
        }
    }
    let mut child = match cmd.spawn() {
        Ok(c) => c,
        Err(e) => {
            eprintln!("wsp: could not start {}: {e}", cmd.get_program().to_string_lossy());
            return 126;
        }
    };
    // After the spawn, so the build does not inherit the ignore: ^C reaches the
    // build through the terminal, and this process must outlive it to release
    // its holds.
    unsafe {
        signal(2, 1);
    }
    match child.wait() {
        Ok(s) => s.code().unwrap_or_else(|| 128 + s.signal().unwrap_or(0)),
        Err(_) => 1,
    }
}

/// `wsp throttle run <tool> args…`, before argument parsing: the args are the
/// tool's, flags and all.
pub(crate) fn run_raw(argv: &[String]) -> i32 {
    let Some((tool, args)) = argv.split_first() else {
        eprintln!("usage: wsp throttle run <tool> [args…]");
        return 2;
    };
    let store = Store::open();
    let idle = hid_idle_secs;
    let say = |m: &str| eprintln!("{m}");
    let env = |k: &str| std::env::var(k).ok().filter(|v| !v.is_empty());
    let ctx = Ctx {
        state: &store.state,
        cwd: std::env::current_dir().unwrap_or_default(),
        path_var: std::env::var("PATH").unwrap_or_default(),
        task: std::env::var("WSP_TASK").unwrap_or_default(),
        cores: cores(),
        idle: &idle,
        alive: &alive,
        say: &say,
        poll: Duration::from_secs(1),
        held: std::env::var_os(HELD).is_some_and(|v| !v.is_empty()),
        env: &env,
    };
    run_in(&ctx, tool, args)
}

// ---- the shims, and the seat's PATH -----------------------------------------

fn shim_text(tool: &str) -> String {
    format!(
        "#!/bin/sh\n\
         # wsp's build throttle (wsp-331): `wsp throttle` says what it is doing.\n\
         if command -v wsp >/dev/null 2>&1; then exec wsp throttle run {tool} \"$@\"; fi\n\
         d=$(cd \"$(dirname \"$0\")\" && pwd)\n\
         PATH=$(printf %s \"$PATH\" | tr ':' '\\n' | grep -vx \"$d\" | paste -sd: -)\n\
         exec {tool} \"$@\"\n"
    )
}

/// Write the shims the settings name, leaving a right one alone.
pub(crate) fn ensure_shims(state: &Path, settings: &Settings) -> PathBuf {
    use std::os::unix::fs::PermissionsExt;
    let dir = shim_dir(state);
    let _ = fs::create_dir_all(&dir);
    for tool in settings.tools.keys() {
        let path = dir.join(tool);
        let text = shim_text(tool);
        if fs::read_to_string(&path).ok().as_deref() == Some(text.as_str()) {
            continue;
        }
        if crate::store::write_atomic(&path, &text).is_ok() {
            let _ = fs::set_permissions(&path, fs::Permissions::from_mode(0o755));
        }
    }
    dir
}

/// What a spawned seat's environment gets: the shims first on `PATH`.
///
/// Only when the spawner's own `PATH` finds a real tool for a shim. A
/// launchd-started daemon has a thin `PATH` and the seat's shell has the
/// person's; replacing the second with the first would take cargo away. When
/// there is nothing to wrap, nothing is set.
pub(crate) fn seat_env() -> BTreeMap<String, String> {
    let state = match std::env::var_os("WSP_STATE") {
        Some(v) => PathBuf::from(v),
        // A test that has not isolated itself must not write shims into the
        // live state directory.
        None if cfg!(test) => return BTreeMap::new(),
        None => Store::default_state(),
    };
    seat_env_in(&state, &std::env::var("PATH").unwrap_or_default())
}

pub(crate) fn seat_env_in(state: &Path, path: &str) -> BTreeMap<String, String> {
    let settings = Settings::load(state);
    let shims = shim_dir(state);
    if !settings.tools.keys().any(|t| real_tool(t, path, &shims).is_some()) {
        return BTreeMap::new();
    }
    let dir = ensure_shims(state, &settings);
    let rest: Vec<&str> = path.split(':').filter(|d| !d.is_empty() && Path::new(d) != dir).collect();
    BTreeMap::from([("PATH".to_string(), format!("{}:{}", dir.display(), rest.join(":")))])
}

// ---- seeing it ----------------------------------------------------------------

pub(crate) struct Build {
    pub holder: Value,
    pub slot: Option<usize>,
}

pub(crate) struct View {
    pub settings: Settings,
    pub idle: Option<u64>,
    pub in_force: bool,
    pub cores: usize,
    pub load: Option<(f64, f64, f64)>,
    pub builds: Vec<Build>,
    pub shims: PathBuf,
}

impl View {
    pub(crate) fn read(state: &Path) -> View {
        let settings = Settings::load(state);
        let idle = match settings.mode {
            Mode::Auto => hid_idle_secs(),
            _ => None,
        };
        let in_force = in_force(settings.mode, idle, settings.away_minutes);
        View {
            settings,
            idle,
            in_force,
            cores: cores(),
            load: load_average(),
            builds: builds(state, &alive),
            shims: shim_dir(state),
        }
    }

    pub(crate) fn load_warning(&self) -> bool {
        self.load.is_some_and(|(one, five, _)| load_high(one, five, self.cores))
    }

    fn force_line(&self) -> String {
        let s = &self.settings;
        match (s.mode, self.in_force) {
            (Mode::Off, _) => "off — builds run at full priority, uncapped, with no slot".into(),
            (Mode::On, _) => "on — in force".into(),
            (Mode::Auto, true) => match self.idle {
                Some(i) => format!("auto — in force, the person is here (idle {})", util::duration_human(i as i64)),
                None => "auto — in force, idle time unreadable so assuming the person is here".into(),
            },
            (Mode::Auto, false) => format!(
                "auto — not in force, away {} (back under {}m)",
                util::duration_human(self.idle.unwrap_or(0) as i64),
                s.away_minutes
            ),
        }
    }

    pub(crate) fn render(&self) -> String {
        let s = &self.settings;
        let mut out = vec![format!("throttle: {}", self.force_line())];
        out.push(format!(
            "  when in force: builds niced +{NICE}, {} job{} each, {} at once machine-wide ({} cores)",
            s.job_cap(self.cores),
            if s.job_cap(self.cores) == 1 { "" } else { "s" },
            s.slot_count(self.cores),
            self.cores
        ));
        out.push("  always: one cargo build per tree".into());
        out.push(format!("  shims: {} (first on every spawned seat's PATH)", util::contract(&self.shims)));
        if let Some((a, b, c)) = self.load {
            out.push(format!("load: {a:.1} {b:.1} {c:.1} on {} cores", self.cores));
        }
        if self.load_warning() {
            out.push(format!("  ⚠ {}", self.load_sentence()));
        }
        match self.builds.is_empty() {
            true => out.push("builds running: none".into()),
            false => {
                out.push("builds running:".into());
                for b in &self.builds {
                    let slot = b.slot.map_or(String::new(), |n| format!("  [slot {}/{}]", n + 1, s.slot_count(self.cores)));
                    let tree = b.holder.get("tree").and_then(Value::as_str).unwrap_or("");
                    out.push(format!("  {}  {}{slot}", who(&b.holder), util::contract(Path::new(tree))));
                }
            }
        }
        out.join("\n")
    }

    /// The one line `wsp wip` and `wsp watch --status` carry in every mode: the
    /// mode, whether it holds this minute, and who is building.
    pub(crate) fn status_line(&self) -> String {
        let building = match self.builds.is_empty() {
            true => String::new(),
            false => format!(
                " · building: {}",
                self.builds.iter().map(|b| who(&b.holder)).collect::<Vec<_>>().join("; ")
            ),
        };
        format!("throttle: {}{building}", self.force_line())
    }

    fn load_sentence(&self) -> String {
        let (one, five, _) = self.load.unwrap_or_default();
        let names: Vec<String> = self.builds.iter().map(|b| who(&b.holder)).collect();
        let building = match names.is_empty() {
            true => "no wsp-run build is among it".to_string(),
            false => format!("builds running: {}", names.join("; ")),
        };
        format!(
            "load is {one:.1} (1m) and {five:.1} (5m) on {} cores, over {LOAD_FACTOR}x for a while — hold off heavy runs; {building}",
            self.cores
        )
    }
}

fn builds(state: &Path, alive: &dyn Fn(u32) -> bool) -> Vec<Build> {
    let read = |name: &str| -> Vec<(String, Value)> {
        let mut v: Vec<(String, Value)> = fs::read_dir(builds_dir(state))
            .into_iter()
            .flatten()
            .flatten()
            .filter(|e| e.file_name().to_string_lossy().starts_with(name))
            .filter_map(|e| {
                let v: Value = serde_json::from_str(&fs::read_to_string(e.path()).ok()?).ok()?;
                alive(holder_pid(&v)).then(|| (e.file_name().to_string_lossy().to_string(), v))
            })
            .collect();
        v.sort_by(|a, b| a.0.cmp(&b.0));
        v
    };
    let slots = read("slot-");
    let mut out: Vec<Build> = read("tree-").into_iter().map(|(_, h)| Build { holder: h, slot: None }).collect();
    for (name, h) in slots {
        let n: usize = name.trim_start_matches("slot-").parse().unwrap_or(0);
        match out.iter_mut().find(|b| holder_pid(&b.holder) == holder_pid(&h)) {
            Some(b) => b.slot = Some(n),
            None => out.push(Build { holder: h, slot: Some(n) }),
        }
    }
    out
}

/// The warning alone, for `wsp wip`: absent while the machine is fine.
pub(crate) fn loud(store: &Store) -> Option<String> {
    let v = View::read(&store.state);
    v.load_warning().then(|| v.load_sentence())
}

/// Whether the load is holding builds back now — the warning [`loud`] words —
/// for what waits on the machine (`wsp-383`). Never under test, where a load
/// reading is noise.
pub(crate) fn holding_back(store: &Store) -> bool {
    !cfg!(test) && loud(store).is_some()
}

/// [`View::status_line`], for the surfaces that are not `wsp throttle`.
pub(crate) fn status_line(store: &Store) -> String {
    View::read(&store.state).status_line()
}

pub(crate) fn health(store: &Store, notes: &mut Vec<String>) {
    let v = View::read(&store.state);
    notes.push(format!("build throttle {}", v.force_line()));
    if v.load_warning() {
        notes.push(format!("⚠ {}", v.load_sentence()));
    }
}

static WARNED: AtomicBool = AtomicBool::new(false);

/// The daemon's half: drop holds whose process is gone, and log the load
/// warning when it starts and when it clears. Not under test; [`reap`] and
/// [`View`] are.
pub(crate) fn tidy(store: &Store) {
    if cfg!(test) {
        return;
    }
    reap(&store.state, &alive);
    let v = View::read(&store.state);
    if v.load_warning() != WARNED.swap(v.load_warning(), Ordering::SeqCst) {
        match v.load_warning() {
            true => crate::cycle::log_line(store, &v.load_sentence()),
            false => crate::cycle::log_line(store, "load is back under the warning line"),
        }
    }
}

pub(crate) fn reap(state: &Path, alive: &dyn Fn(u32) -> bool) -> usize {
    let mut n = 0;
    for e in fs::read_dir(builds_dir(state)).into_iter().flatten().flatten() {
        let path = e.path();
        if path.extension().is_some() {
            continue;
        }
        let dead = match fs::read_to_string(&path).ok().and_then(|t| serde_json::from_str::<Value>(&t).ok()) {
            Some(v) => !alive(holder_pid(&v)),
            None => false,
        };
        if dead && fs::remove_file(&path).is_ok() {
            n += 1;
        }
    }
    n
}

// ---- the verb -----------------------------------------------------------------

pub(crate) fn throttle(store: &Store, args: &Args) -> i32 {
    let mut s = Settings::load(&store.state);
    let words: Vec<&str> = args.rest.iter().map(String::as_str).collect();
    match words.as_slice() {
        [] => {}
        [m @ ("off" | "on" | "auto")] => {
            s.mode = Mode::parse(m).unwrap_or(Mode::Auto);
            if let Err(e) = s.save(&store.state) {
                eprintln!("wsp: could not write the throttle setting: {e}");
                return 1;
            }
        }
        ["set", pairs @ ..] if !pairs.is_empty() => {
            for p in pairs {
                let Some((k, v)) = p.split_once('=') else {
                    eprintln!("wsp: `{p}` is not k=v — slots=N jobs=N away=MINUTES, or `auto` for the first two");
                    return 2;
                };
                let n = || v.parse::<usize>().ok().filter(|n| *n > 0);
                match (k, v) {
                    ("slots", "auto") => s.slots = None,
                    ("jobs", "auto") => s.jobs = None,
                    ("slots", _) => match n() {
                        Some(n) => s.slots = Some(n),
                        None => return bad(k, v),
                    },
                    ("jobs", _) => match n() {
                        Some(n) => s.jobs = Some(n),
                        None => return bad(k, v),
                    },
                    ("away", _) => match v.parse::<u64>() {
                        Ok(n) => s.away_minutes = n,
                        Err(_) => return bad(k, v),
                    },
                    _ => {
                        eprintln!("wsp: no throttle setting `{k}` — slots, jobs, away");
                        return 2;
                    }
                }
            }
            if let Err(e) = s.save(&store.state) {
                eprintln!("wsp: could not write the throttle setting: {e}");
                return 1;
            }
        }
        _ => {
            eprintln!("usage: wsp throttle [off|on|auto | set slots=N jobs=N away=MINUTES]   (bare: what it is doing now)");
            return 2;
        }
    }
    ensure_shims(&store.state, &s);
    let v = View::read(&store.state);
    match args.json() {
        true => println!(
            "{}",
            serde_json::to_string_pretty(&json!({
                "mode": v.settings.mode.word(),
                "in_force": v.in_force,
                "idle_secs": v.idle,
                "cores": v.cores,
                "slots": v.settings.slot_count(v.cores),
                "jobs": v.settings.job_cap(v.cores),
                "load": v.load.map(|(a, b, c)| json!([a, b, c])),
                "load_warning": v.load_warning(),
                "builds": v.builds.iter().map(|b| json!({"holder": b.holder, "slot": b.slot})).collect::<Vec<_>>(),
            }))
            .unwrap_or_default()
        ),
        false => println!("{}", v.render()),
    }
    0
}

fn bad(k: &str, v: &str) -> i32 {
    eprintln!("wsp: `{k}={v}` is not a number worth setting");
    2
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::RefCell;

    fn s(w: &[&str]) -> Vec<String> {
        w.iter().map(|x| x.to_string()).collect()
    }

    #[test]
    fn auto_holds_while_the_person_is_here_and_lifts_when_they_have_been_away() {
        assert!(in_force(Mode::Auto, Some(20), 5));
        assert!(!in_force(Mode::Auto, Some(5 * 60), 5));
        assert!(!in_force(Mode::Auto, Some(3 * 3600), 5));
    }

    #[test]
    fn auto_that_cannot_read_the_idle_time_holds() {
        assert!(in_force(Mode::Auto, None, 5));
    }

    #[test]
    fn on_and_off_ignore_the_idle_time() {
        assert!(in_force(Mode::On, Some(99_999), 5));
        assert!(!in_force(Mode::Off, Some(0), 5));
    }

    #[test]
    fn the_hid_idle_time_is_read_out_of_ioreg_in_seconds() {
        let out = "    | |   \"HIDIdleTime\" = 125000000000\n    | |   \"Other\" = 1\n";
        assert_eq!(parse_hid_idle(out), Some(125));
        assert_eq!(parse_hid_idle("nothing here"), None);
    }

    #[test]
    fn the_readiness_wait_stretches_with_the_load_per_core_and_stops_at_four() {
        assert_eq!(readiness_scale(0.5, 8), 1);
        assert_eq!(readiness_scale(8.0, 8), 1, "a full machine is not a loaded one");
        assert_eq!(readiness_scale(9.0, 8), 2);
        assert_eq!(readiness_scale(20.0, 8), 3);
        assert_eq!(readiness_scale(60.0, 8), 4, "capped: past it the start is ended and tried again");
        assert_eq!(readiness_scale(5.0, 0), 4, "no core count is not a divide by zero");
    }

    #[test]
    fn the_load_average_reads_from_proc_and_from_sysctl() {
        assert_eq!(parse_loadavg("0.50 0.40 0.30 1/200 99"), Some((0.5, 0.4, 0.3)));
        assert_eq!(parse_loadavg("{ 48.1 30.2 12.0 }"), Some((48.1, 30.2, 12.0)));
        assert_eq!(parse_loadavg("garbage"), None);
    }

    #[test]
    fn a_spike_that_has_not_lasted_is_not_a_load_warning() {
        assert!(load_high(48.0, 30.0, 8));
        assert!(!load_high(48.0, 3.0, 8), "the 5-minute average says it just began");
        assert!(!load_high(11.0, 30.0, 8), "the 1-minute average says it has passed");
        assert!(!load_high(12.0, 12.0, 8), "exactly 1.5x is not over it");
    }

    #[test]
    fn only_a_subcommand_that_compiles_and_finishes_holds_anything() {
        assert_eq!(weight("cargo", &s(&["test", "-p", "x"])), Weight::Heavy);
        assert_eq!(weight("cargo", &s(&["+nightly", "build", "--release"])), Weight::Heavy);
        assert_eq!(weight("cargo", &s(&["-q", "check"])), Weight::Heavy);
        assert_eq!(weight("cargo", &s(&["run"])), Weight::Runs, "a run may stay up for hours and would hold the tree");
        assert_eq!(weight("cargo", &s(&["metadata"])), Weight::Light);
        assert_eq!(weight("cargo", &s(&["--version"])), Weight::Light);
        assert_eq!(weight("make", &s(&["all"])), Weight::Heavy, "a declared tool is heavy whatever it is asked");
    }

    #[test]
    fn the_default_caps_leave_the_person_two_cores_on_eight() {
        let st = Settings::default();
        assert_eq!(st.slot_count(8), 2);
        assert_eq!(st.job_cap(8), 3);
        assert_eq!(st.slot_count(2), 1);
        assert_eq!(st.job_cap(1), 1);
        let set = Settings { slots: Some(4), jobs: Some(2), ..Settings::default() };
        assert_eq!((set.slot_count(8), set.job_cap(8)), (4, 2));
    }

    #[test]
    fn the_setting_survives_a_restart_and_defaults_to_auto() {
        let t = crate::util::isolated("throttle-settings");
        assert_eq!(Settings::load(&t.state()).mode, Mode::Auto);
        let mut st = Settings::default();
        st.mode = Mode::Off;
        st.slots = Some(3);
        st.save(&t.state()).unwrap();
        let back = Settings::load(&t.state());
        assert_eq!((back.mode, back.slots, back.jobs), (Mode::Off, Some(3), None));
    }

    // ---- holds ----

    fn dead_pid() -> u32 {
        let mut c = Command::new("true").spawn().unwrap();
        let pid = c.id();
        c.wait().unwrap();
        pid
    }

    fn rec_for(pid: u32) -> Value {
        json!({"pid": pid, "task": "wsp-9", "cmd": "cargo test", "tree": "/t", "since": util::epoch_secs()})
    }

    #[test]
    fn a_live_hold_is_waited_on_and_a_dead_one_is_taken_over() {
        let t = crate::util::isolated("throttle-hold");
        let p = slot_path(&t.state(), 0);
        let me = std::process::id();
        let live = |pid: u32| pid == me;
        let first = take(&p, &rec_for(me), &live);
        assert!(matches!(first, Taken::Yours(_)));
        // A second taker sees it held by us.
        let other = take(&p, &rec_for(1), &live);
        assert!(matches!(other, Taken::HeldBy(ref v) if holder_pid(v) == me));
        drop(first);
        assert!(!p.exists(), "dropping a hold removes it");

        let ghost = dead_pid();
        fs::create_dir_all(p.parent().unwrap()).unwrap();
        fs::write(&p, rec_for(ghost).to_string()).unwrap();
        let took = take(&p, &rec_for(me), &live);
        assert!(matches!(took, Taken::Yours(_)), "a hold whose process is gone is not a hold");
        assert!(fs::read_to_string(&p).unwrap().contains(&format!("\"pid\":{me}")));
    }

    #[test]
    fn a_hold_that_was_never_filled_in_is_waited_on_until_it_is_old() {
        let t = crate::util::isolated("throttle-empty");
        let p = slot_path(&t.state(), 0);
        fs::create_dir_all(p.parent().unwrap()).unwrap();
        fs::write(&p, "").unwrap();
        let me = std::process::id();
        assert!(matches!(take(&p, &rec_for(me), &|_| true), Taken::HeldBy(_)), "a creator is mid-write");
    }

    #[test]
    fn dropping_a_hold_never_removes_someone_elses() {
        let t = crate::util::isolated("throttle-not-mine");
        let p = slot_path(&t.state(), 0);
        let hold = match take(&p, &rec_for(std::process::id()), &|_| true) {
            Taken::Yours(h) => h,
            _ => panic!("free"),
        };
        fs::write(&p, rec_for(dead_pid()).to_string()).unwrap();
        drop(hold);
        assert!(p.exists(), "the file was taken over by another holder");
    }

    #[test]
    fn the_daemon_reaps_holds_whose_process_is_gone_and_only_those() {
        let t = crate::util::isolated("throttle-reap");
        let dir = builds_dir(&t.state());
        fs::create_dir_all(&dir).unwrap();
        let me = std::process::id();
        fs::write(dir.join("slot-0"), rec_for(me).to_string()).unwrap();
        fs::write(dir.join("slot-1"), rec_for(dead_pid()).to_string()).unwrap();
        assert_eq!(reap(&t.state(), &|p| p == me), 1);
        assert!(dir.join("slot-0").exists() && !dir.join("slot-1").exists());
    }

    #[test]
    fn a_tree_is_the_nearest_directory_with_a_git_entry() {
        let t = crate::util::isolated("throttle-tree");
        let root = t.path("repo");
        fs::create_dir_all(root.join("a/b")).unwrap();
        fs::write(root.join(".git"), "gitdir: x").unwrap();
        assert_eq!(tree_of(&root.join("a/b")), fs::canonicalize(&root).unwrap());
    }

    // ---- running a build against a fake tool ----

    struct Rig {
        _iso: crate::util::Isolated,
        bin: PathBuf,
        out: PathBuf,
        state: PathBuf,
        tree: PathBuf,
    }

    /// A `cargo` that records the job cap it was given and the niceness it ran
    /// at, then takes `$FAKE_SLEEP` seconds.
    fn rig(name: &str) -> Rig {
        use std::os::unix::fs::PermissionsExt;
        let iso = crate::util::isolated(name);
        // `wsp verify` runs these tests under a shim of its own, so the process
        // is born holding a tree and a job cap; the fake tool must see only
        // what the run under test gave it.
        std::env::remove_var(HELD);
        std::env::remove_var("CARGO_BUILD_JOBS");
        let bin = iso.path("bin");
        fs::create_dir_all(&bin).unwrap();
        let out = iso.path("seen");
        let script = format!(
            "#!/bin/sh\necho \"$$ jobs=${{CARGO_BUILD_JOBS:-none}} nice=$(ps -o ni= -p $$ | tr -d ' ') held=${{{HELD}:+yes}} args=$*\" >> {out}\n[ -n \"$FAKE_SLEEP\" ] && sleep \"$FAKE_SLEEP\"\nexit 0\n",
            out = out.display()
        );
        fs::write(bin.join("cargo"), script).unwrap();
        fs::set_permissions(bin.join("cargo"), fs::Permissions::from_mode(0o755)).unwrap();
        let tree = iso.path("tree");
        fs::create_dir_all(&tree).unwrap();
        fs::write(tree.join(".git"), "gitdir: x").unwrap();
        let state = iso.state();
        Rig { _iso: iso, bin, out, state, tree }
    }

    fn mode(rig: &Rig, m: Mode, slots: usize) {
        let st = Settings { mode: m, slots: Some(slots), jobs: Some(2), ..Settings::default() };
        st.save(&rig.state).unwrap();
    }

    fn ctx<'a>(
        rig: &'a Rig,
        idle: &'a dyn Fn() -> Option<u64>,
        say: &'a dyn Fn(&str),
        env: &'a dyn Fn(&str) -> Option<String>,
    ) -> Ctx<'a> {
        Ctx {
            state: &rig.state,
            cwd: rig.tree.clone(),
            path_var: format!("{}:/usr/bin:/bin", rig.bin.display()),
            task: "wsp-9".into(),
            cores: 8,
            idle,
            alive: &alive,
            say,
            poll: Duration::from_millis(20),
            held: false,
            env,
        }
    }

    /// The niceness the test process itself runs at: `nice -n 12 cargo test`
    /// hands every fake cargo that base, throttled or not.
    fn base_nice() -> i32 {
        let out = Command::new("ps")
            .args(["-o", "ni=", "-p", &std::process::id().to_string()])
            .output()
            .unwrap();
        String::from_utf8_lossy(&out.stdout).trim().parse().unwrap()
    }

    fn nice_of(line: &str) -> i32 {
        let rest = line.split("nice=").nth(1).unwrap_or_else(|| panic!("no nice in {line}"));
        rest.split_whitespace().next().unwrap().parse().unwrap()
    }

    /// Niced by the throttle: setpriority sets NICE, and never lowers a runner
    /// already above it.
    fn niced(line: &str) -> bool {
        nice_of(line) == base_nice().max(NICE)
    }

    /// Left at the niceness it was born with, whatever that is.
    fn at_base_nice(line: &str) -> bool {
        nice_of(line) == base_nice()
    }

    fn seen(rig: &Rig) -> Vec<String> {
        fs::read_to_string(&rig.out).unwrap_or_default().lines().map(String::from).collect()
    }

    #[test]
    fn a_build_with_the_throttle_in_force_runs_niced_under_the_job_cap() {
        let rig = rig("throttle-run-on");
        mode(&rig, Mode::On, 2);
        let (idle, say, env) = (|| Some(0), |_: &str| {}, |_: &str| None);
        assert_eq!(run_in(&ctx(&rig, &idle, &say, &env), "cargo", &s(&["test"])), 0);
        let line = &seen(&rig)[0];
        assert!(line.contains("jobs=2") && niced(line), "{line}");
    }

    #[test]
    fn a_job_cap_the_agent_set_lower_is_kept() {
        let rig = rig("throttle-run-lower");
        mode(&rig, Mode::On, 2);
        let (idle, say) = (|| Some(0), |_: &str| {});
        let env = |k: &str| (k == "CARGO_BUILD_JOBS").then(|| "1".to_string());
        run_in(&ctx(&rig, &idle, &say, &env), "cargo", &s(&["build"]));
        assert!(seen(&rig)[0].contains("jobs=1"));
    }

    #[test]
    fn auto_with_the_person_away_runs_at_full_speed() {
        let rig = rig("throttle-run-away");
        mode(&rig, Mode::Auto, 2);
        let (idle, say, env) = (|| Some(3600), |_: &str| {}, |_: &str| None);
        run_in(&ctx(&rig, &idle, &say, &env), "cargo", &s(&["build"]));
        let line = &seen(&rig)[0];
        assert!(line.contains("jobs=none") && at_base_nice(line), "{line}");
        assert!(
            !builds_dir(&rig.state).join("slot-0").exists(),
            "an away machine takes no slot, and the hold is gone after"
        );
    }

    #[test]
    fn auto_with_the_person_at_the_machine_throttles() {
        let rig = rig("throttle-run-here");
        mode(&rig, Mode::Auto, 2);
        let (idle, say, env) = (|| Some(10), |_: &str| {}, |_: &str| None);
        run_in(&ctx(&rig, &idle, &say, &env), "cargo", &s(&["check"]));
        assert!(niced(&seen(&rig)[0]));
    }

    #[test]
    fn off_never_throttles_and_still_holds_the_tree() {
        let rig = rig("throttle-run-off");
        mode(&rig, Mode::Off, 2);
        let tree = tree_of(&rig.tree);
        let (idle, say, env) = (|| Some(0), |_: &str| {}, |_: &str| None);
        run_in(&ctx(&rig, &idle, &say, &env), "cargo", &s(&["test"]));
        let line = &seen(&rig)[0];
        assert!(line.contains("jobs=none") && at_base_nice(line), "{line}");
        // And the tree hold was taken and released: a second build is not stuck.
        assert!(!tree_path(&rig.state, &tree).exists());
    }

    #[test]
    fn a_nested_build_passes_through_without_a_hold() {
        let rig = rig("throttle-run-nested");
        mode(&rig, Mode::On, 1);
        let me = std::process::id();
        // Both the only slot and the tree held by someone alive: a nested build
        // that waited would wait forever.
        fs::create_dir_all(builds_dir(&rig.state)).unwrap();
        fs::write(slot_path(&rig.state, 0), rec_for(me).to_string()).unwrap();
        fs::write(tree_path(&rig.state, &tree_of(&rig.tree)), rec_for(me).to_string()).unwrap();
        let (idle, say, env) = (|| Some(0), |_: &str| {}, |_: &str| None);
        let mut c = ctx(&rig, &idle, &say, &env);
        c.held = true;
        assert_eq!(run_in(&c, "cargo", &s(&["build"])), 0);
        assert_eq!(seen(&rig).len(), 1);
    }

    #[test]
    fn a_second_build_in_one_tree_waits_for_the_first_in_every_mode() {
        for (m, name) in [(Mode::Off, "throttle-tree-off"), (Mode::On, "throttle-tree-on")] {
            let rig = rig(name);
            mode(&rig, m, 4);
            let tree = tree_of(&rig.tree);
            let me = std::process::id();
            fs::create_dir_all(builds_dir(&rig.state)).unwrap();
            let held = tree_path(&rig.state, &tree);
            fs::write(&held, rec_for(me).to_string()).unwrap();
            let said = RefCell::new(Vec::<String>::new());
            let (idle, say, env) = (|| Some(0), |m: &str| said.borrow_mut().push(m.to_string()), |_: &str| None);
            let freer = {
                let held = held.clone();
                std::thread::spawn(move || {
                    std::thread::sleep(Duration::from_millis(300));
                    fs::remove_file(held).unwrap();
                })
            };
            let started = std::time::Instant::now();
            run_in(&ctx(&rig, &idle, &say, &env), "cargo", &s(&["build"]));
            freer.join().unwrap();
            assert!(started.elapsed() >= Duration::from_millis(250), "{name}: it did not wait");
            assert_eq!(seen(&rig).len(), 1);
            let said = said.borrow();
            assert!(said[0].contains("its tree") && said[0].contains("wsp-9"), "{name}: {said:?}");
        }
    }

    #[test]
    fn a_second_heavy_build_waits_for_a_slot_and_says_who_has_it() {
        let rig = rig("throttle-slot");
        mode(&rig, Mode::On, 1);
        let me = std::process::id();
        fs::create_dir_all(builds_dir(&rig.state)).unwrap();
        let held = slot_path(&rig.state, 0);
        fs::write(&held, rec_for(me).to_string()).unwrap();
        let said = RefCell::new(Vec::<String>::new());
        let (idle, say, env) = (|| Some(0), |m: &str| said.borrow_mut().push(m.to_string()), |_: &str| None);
        let freer = {
            let held = held.clone();
            std::thread::spawn(move || {
                std::thread::sleep(Duration::from_millis(300));
                fs::remove_file(held).unwrap();
            })
        };
        let started = std::time::Instant::now();
        run_in(&ctx(&rig, &idle, &say, &env), "cargo", &s(&["test", "x::"]));
        freer.join().unwrap();
        assert!(started.elapsed() >= Duration::from_millis(250));
        assert!(said.borrow()[0].contains("a build slot") && said.borrow()[0].contains("Queued, not failed"));
    }

    #[test]
    fn a_wait_for_a_slot_ends_when_auto_lifts_because_the_person_left() {
        let rig = rig("throttle-lift");
        mode(&rig, Mode::Auto, 1);
        let me = std::process::id();
        fs::create_dir_all(builds_dir(&rig.state)).unwrap();
        fs::write(slot_path(&rig.state, 0), rec_for(me).to_string()).unwrap();
        let polls = std::cell::Cell::new(0);
        let idle = || {
            polls.set(polls.get() + 1);
            // Here for the first look, away for every look after.
            Some(if polls.get() == 1 { 5 } else { 9999 })
        };
        let (say, env) = (|_: &str| {}, |_: &str| None);
        run_in(&ctx(&rig, &idle, &say, &env), "cargo", &s(&["test"]));
        let line = &seen(&rig)[0];
        assert!(line.contains("jobs=none") && at_base_nice(line), "{line}");
    }

    #[test]
    fn a_build_that_is_not_heavy_is_neither_held_nor_niced() {
        let rig = rig("throttle-light");
        mode(&rig, Mode::On, 1);
        let me = std::process::id();
        fs::create_dir_all(builds_dir(&rig.state)).unwrap();
        fs::write(slot_path(&rig.state, 0), rec_for(me).to_string()).unwrap();
        let (idle, say, env) = (|| Some(0), |_: &str| {}, |_: &str| None);
        assert_eq!(run_in(&ctx(&rig, &idle, &say, &env), "cargo", &s(&["metadata"])), 0);
        assert!(seen(&rig)[0].contains("jobs=none"));
    }

    #[test]
    fn cargo_run_is_niced_and_capped_but_holds_nothing() {
        let rig = rig("throttle-run-runs");
        mode(&rig, Mode::On, 1);
        let me = std::process::id();
        fs::create_dir_all(builds_dir(&rig.state)).unwrap();
        fs::write(slot_path(&rig.state, 0), rec_for(me).to_string()).unwrap();
        let (idle, say, env) = (|| Some(0), |_: &str| {}, |_: &str| None);
        run_in(&ctx(&rig, &idle, &say, &env), "cargo", &s(&["run", "-p", "host"]));
        let line = &seen(&rig)[0];
        assert!(line.contains("jobs=2") && niced(line) && !line.contains("held=yes"), "{line}");
    }

    #[test]
    fn a_cargo_test_with_no_filter_is_the_full_suite_and_a_filtered_one_is_not() {
        let a = |v: &[&str]| v.iter().map(|s| s.to_string()).collect::<Vec<_>>();
        assert!(is_full_suite("cargo", &a(&["test"])));
        assert!(is_full_suite("cargo", &a(&["+nightly", "test", "--release"])));
        assert!(is_full_suite("cargo", &a(&["test", "--", "--nocapture"])));
        assert!(!is_full_suite("cargo", &a(&["test", "cmd_spawn::"])));
        assert!(!is_full_suite("cargo", &a(&["test", "--lib"])));
        assert!(!is_full_suite("cargo", &a(&["test", "-p", "x"])));
        assert!(!is_full_suite("cargo", &a(&["test", "--", "name"])));
        assert!(!is_full_suite("cargo", &a(&["build"])));
        assert!(!is_full_suite("npm", &a(&["test"])));
    }

    #[test]
    fn a_full_suite_in_a_rows_seat_is_noted_and_counted_and_a_filtered_one_is_not() {
        let rig = rig("throttle-full-suite");
        mode(&rig, Mode::Off, 1);
        let idle = || None;
        let said = std::cell::RefCell::new(Vec::<String>::new());
        let say = |m: &str| said.borrow_mut().push(m.to_string());
        let env = |k: &str| (k == crate::place::SEAT_ENV).then(|| "w1:p1".to_string());
        let c = ctx(&rig, &idle, &say, &env);
        run_in(&c, "cargo", &["test".into(), "cmd_spawn::".into()]);
        assert_eq!(full_suite_runs(&rig.state, "wsp-9"), 0, "a filtered run is the member's own");
        run_in(&c, "cargo", &["test".into()]);
        run_in(&c, "cargo", &["test".into()]);
        assert_eq!(full_suite_runs(&rig.state, "wsp-9"), 2);
        assert_eq!(full_suite_seats(&rig.state), std::collections::BTreeSet::from(["w1:p1".to_string()]));
        assert!(said.borrow().iter().any(|m| m.contains("full suite") && m.contains("barrier")), "{:?}", said.borrow());
        let mut seatless = ctx(&rig, &idle, &say, &env);
        seatless.task = String::new();
        run_in(&seatless, "cargo", &["test".into()]);
        assert_eq!(full_suite_runs(&rig.state, ""), 0, "no row, no note");
    }

    #[test]
    fn the_exit_code_of_the_build_is_the_exit_code_of_the_shim() {
        use std::os::unix::fs::PermissionsExt;
        let rig = rig("throttle-exit");
        mode(&rig, Mode::Off, 1);
        fs::write(rig.bin.join("cargo"), "#!/bin/sh\nexit 101\n").unwrap();
        fs::set_permissions(rig.bin.join("cargo"), fs::Permissions::from_mode(0o755)).unwrap();
        let (idle, say, env) = (|| Some(0), |_: &str| {}, |_: &str| None);
        assert_eq!(run_in(&ctx(&rig, &idle, &say, &env), "cargo", &s(&["test"])), 101);
    }

    // ---- the seat's environment ----

    #[test]
    fn a_seat_gets_the_shims_first_on_its_path_and_the_real_tool_still_resolves() {
        let rig = rig("throttle-seat");
        let path = format!("{}:/usr/bin:/bin", rig.bin.display());
        let env = seat_env_in(&rig.state, &path);
        let got = &env["PATH"];
        let shims = shim_dir(&rig.state);
        assert!(got.starts_with(&format!("{}:", shims.display())), "{got}");
        assert!(got.ends_with(&path), "the rest of the PATH is kept whole: {got}");
        assert!(shims.join("cargo").exists());
        assert_eq!(real_tool("cargo", got, &shims), Some(rig.bin.join("cargo")), "the shim does not find itself");
    }

    #[test]
    fn a_shim_from_another_state_dir_is_not_the_real_tool() {
        let rig = rig("throttle-foreign-shim");
        let other = rig.state.join("other-state");
        let foreign = ensure_shims(&other, &Settings::load(&other));
        let path = format!("{}:{}:/usr/bin", foreign.display(), rig.bin.display());
        let mine = shim_dir(&rig.state);
        assert_eq!(real_tool("cargo", &path, &mine), Some(rig.bin.join("cargo")));
        let only = format!("{}:/usr/bin", foreign.display());
        assert_eq!(real_tool("cargo", &only, &mine).filter(|p| p.starts_with(&foreign)), None);
    }

    #[test]
    fn a_spawner_with_no_cargo_on_its_path_leaves_the_seats_path_alone() {
        let rig = rig("throttle-no-cargo");
        assert!(seat_env_in(&rig.state, "/usr/bin:/bin").is_empty());
    }

    #[test]
    fn the_shim_execs_wsp_throttle_run_and_falls_back_to_the_real_tool() {
        let text = shim_text("cargo");
        assert!(text.contains("exec wsp throttle run cargo \"$@\""));
        assert!(text.contains("exec cargo \"$@\""), "a seat with no wsp on its path still builds");
    }

    #[test]
    fn the_view_names_the_builds_running_and_who_they_are_for() {
        let t = crate::util::isolated("throttle-view");
        let me = std::process::id();
        fs::create_dir_all(builds_dir(&t.state())).unwrap();
        fs::write(slot_path(&t.state(), 0), rec_for(me).to_string()).unwrap();
        fs::write(tree_path(&t.state(), Path::new("/t")), rec_for(me).to_string()).unwrap();
        let v = View {
            settings: Settings::default(),
            idle: Some(12),
            in_force: true,
            cores: 8,
            load: Some((20.0, 18.0, 10.0)),
            builds: builds(&t.state(), &|p| p == me),
            shims: PathBuf::from("/s"),
        };
        assert_eq!(v.builds.len(), 1, "one build, holding a tree and a slot");
        let out = v.render();
        assert!(out.contains("auto — in force, the person is here"), "{out}");
        assert!(out.contains("wsp-9 `cargo test`") && out.contains("[slot 1/2]"), "{out}");
        assert!(out.contains("⚠ load is 20.0 (1m) and 18.0 (5m) on 8 cores"), "{out}");
    }

    #[test]
    fn the_status_line_names_the_mode_and_whether_it_holds_in_every_mode() {
        for (m, idle, want) in [
            (Mode::Off, Some(0), "throttle: off"),
            (Mode::On, Some(9999), "throttle: on — in force"),
            (Mode::Auto, Some(12), "throttle: auto — in force, the person is here"),
            (Mode::Auto, Some(9999), "throttle: auto — not in force, away"),
        ] {
            let v = View {
                settings: Settings { mode: m, ..Settings::default() },
                idle,
                in_force: in_force(m, idle, 5),
                cores: 8,
                load: Some((1.0, 1.0, 1.0)),
                builds: vec![],
                shims: PathBuf::from("/s"),
            };
            assert!(v.status_line().starts_with(want), "{m:?}: {}", v.status_line());
            assert!(!v.load_warning(), "the line shows with no load warning up");
        }
    }

    #[test]
    fn the_status_line_names_who_is_building() {
        let v = View {
            settings: Settings::default(),
            idle: Some(1),
            in_force: true,
            cores: 8,
            load: None,
            builds: vec![Build { holder: rec_for(1), slot: Some(0) }],
            shims: PathBuf::from("/s"),
        };
        assert!(v.status_line().contains(" · building: wsp-9 `cargo test`"), "{}", v.status_line());
    }

    #[test]
    fn the_load_warning_shows_whatever_the_mode() {
        for m in [Mode::Off, Mode::On, Mode::Auto] {
            let v = View {
                settings: Settings { mode: m, ..Settings::default() },
                idle: None,
                in_force: false,
                cores: 8,
                load: Some((30.0, 25.0, 20.0)),
                builds: vec![],
                shims: PathBuf::from("/s"),
            };
            assert!(v.load_warning() && v.render().contains("hold off heavy runs"), "{m:?}");
        }
    }
}
