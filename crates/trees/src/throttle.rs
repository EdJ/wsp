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
use crate::host::Host;
use std::process::Stdio;
use wsp_base::calls::Command;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use serde_json::{json, Value};

use crate::store::Store;
use crate::host::LOCAL;
use crate::util;
use crate::base::args::Args;

/// Set in the environment of a build wsp is holding, so a build nested in it
/// passes through. `WSP_`-prefixed so a spawned seat sheds it
/// ([`crate::cmd_spawn`]'s `unowned`) and is never born inside someone's hold.
pub const HELD: &str = "WSP_BUILD_HELD";

/// How much a throttled build is niced by.
const NICE: i32 = 12;

/// Minutes without a keypress or mouse move after which the person has left.
const AWAY_MINUTES: u64 = 5;

/// Load over this many times the core count, in both the 1- and 5-minute
/// average, is a machine nobody can work on.
const LOAD_FACTOR: f64 = 1.5;

/// `CPU_Speed_Limit` (percent of the rated clock, `pmset -g therm`) under which
/// the machine is thermally limited (`wsp-347`). Ed's i7-8559U read 31 under a
/// fleet of builds; a cool one reads 100, and it sat at 82 with a light load.
const THERMAL_BELOW: u32 = 70;

/// What a thermally limited machine is held to: heavy builds at once, and jobs
/// in each, as measured in `compounding/docs/fast-loop/thermal.md` on the
/// 4-core, 8-thread i7-8559U: four builds of two jobs finished the most per
/// hour (about 135) once it was hot, against about 95 for two of four and 55
/// for one of eight. A warm edit's build is mostly one rustc and a link, so
/// the slots matter more than the jobs; the total jobs stay at the threads.
const HOT_SLOTS: usize = 4;
const HOT_JOBS: usize = 2;

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
pub enum Mode {
    Off,
    On,
    Auto,
}

impl Mode {
    pub fn word(self) -> &'static str {
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
pub struct Settings {
    pub mode: Mode,
    /// Heavy builds at once across the fleet. `None` is a quarter of the cores.
    pub slots: Option<usize>,
    /// Parallel jobs in one build. `None` is the cores, less one, shared out
    /// across the slots.
    pub jobs: Option<usize>,
    pub away_minutes: u64,
    /// `CPU_Speed_Limit` under which the machine counts as thermally limited.
    pub thermal_below: u32,
    /// Slots and jobs while it is. `None` is the measured best ([`HOT_SLOTS`],
    /// [`HOT_JOBS`]), kept under whatever `slots` and `jobs` a hand set.
    pub hot_slots: Option<usize>,
    pub hot_jobs: Option<usize>,
    /// Checks, verifier builds and barrier runs at once machine-wide
    /// (`wsp-668`). `None` is half the cores, between 2 and 8.
    pub checks: Option<usize>,
    /// Tool name to the variable that caps its parallelism. Edited by hand in
    /// the file; the default is cargo.
    pub tools: BTreeMap<String, String>,
}

impl Default for Settings {
    fn default() -> Settings {
        Settings {
            mode: Mode::Auto,
            slots: None,
            jobs: None,
            away_minutes: AWAY_MINUTES,
            thermal_below: THERMAL_BELOW,
            hot_slots: None,
            hot_jobs: None,
            checks: None,
            tools: default_tools(),
        }
    }
}

fn settings_path(state: &Path) -> PathBuf {
    state.join("throttle.json")
}

impl Settings {
    pub fn load(state: &Path) -> Settings {
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
        if let Some(n) = v.get("thermal_below").and_then(Value::as_u64) {
            s.thermal_below = n.min(100) as u32;
        }
        s.hot_slots = v.get("hot_slots").and_then(Value::as_u64).map(|n| n.max(1) as usize);
        s.hot_jobs = v.get("hot_jobs").and_then(Value::as_u64).map(|n| n.max(1) as usize);
        s.checks = v.get("checks").and_then(Value::as_u64).map(|n| n.max(1) as usize);
        if let Some(t) = v.get("tools").and_then(Value::as_object) {
            let tools: BTreeMap<String, String> =
                t.iter().filter_map(|(k, v)| v.as_str().map(|v| (k.clone(), v.to_string()))).collect();
            if !tools.is_empty() {
                s.tools = tools;
            }
        }
        s
    }

    pub fn save(&self, state: &Path) -> std::io::Result<()> {
        let _ = fs::create_dir_all(state);
        let v = json!({
            "mode": self.mode.word(),
            "slots": self.slots,
            "jobs": self.jobs,
            "away_minutes": self.away_minutes,
            "thermal_below": self.thermal_below,
            "hot_slots": self.hot_slots,
            "hot_jobs": self.hot_jobs,
            "checks": self.checks,
            "tools": self.tools,
        });
        crate::store::write_atomic(&settings_path(state), &serde_json::to_string_pretty(&v).unwrap_or_default())
    }

    pub fn slot_count(&self, cores: usize) -> usize {
        self.slots.unwrap_or((cores / 4).max(1))
    }

    pub fn job_cap(&self, cores: usize) -> usize {
        self.jobs.unwrap_or_else(|| (cores.saturating_sub(1) / self.slot_count(cores)).max(1))
    }

    /// Whether a `CPU_Speed_Limit` reading is a thermally limited machine.
    /// Unreadable is not: the reading is only there on a Mac, and a cool
    /// machine must not be held back for want of one.
    pub fn is_hot(&self, speed_limit: Option<u32>) -> bool {
        speed_limit.is_some_and(|l| l < self.thermal_below)
    }

    /// [`Settings::slot_count`], or while the machine is hot the measured best,
    /// which is not the ordinary count's to cap: a quarter of the cores is
    /// fewer builds than the machine finishes most with. A hand's `slots` and
    /// `jobs` still are a ceiling.
    pub fn slots_at(&self, cores: usize, hot: bool) -> usize {
        match hot {
            true => self.hot_slots.unwrap_or_else(|| self.slots.map_or(HOT_SLOTS, |s| s.min(HOT_SLOTS))),
            false => self.slot_count(cores),
        }
    }

    pub fn jobs_at(&self, cores: usize, hot: bool) -> usize {
        match hot {
            true => self.hot_jobs.unwrap_or_else(|| self.jobs.map_or(HOT_JOBS, |j| j.min(HOT_JOBS))),
            false => self.job_cap(cores),
        }
    }

    /// Checks, verifier builds and barrier runs at once (`wsp-668`): the seat
    /// cap holds agents only, and each wide group starts its members' checks
    /// beside them. While the clock is limited the count is held to the hot
    /// slots, so a machine that has more cores than it can cool runs fewer.
    pub fn check_slots_at(&self, cores: usize, hot: bool) -> usize {
        let cool = self.checks.unwrap_or_else(|| (cores / 2).clamp(2, 8));
        match hot {
            true => cool.min(self.hot_slots.unwrap_or(HOT_SLOTS)).max(1),
            false => cool,
        }
    }

    /// The agent cap to suggest on a hot machine: one for each slot, so a
    /// finished step finds a build slot free more often than not. Not measured;
    /// the file says so.
    pub fn hot_agents(&self, cores: usize) -> usize {
        self.slots_at(cores, true).max(1)
    }
}

/// Whether the throttle holds right now. When `auto` cannot read the idle time
/// it holds: wrongly throttling an idle machine costs minutes, and wrongly
/// not throttling one with a person at it costs the person.
pub fn in_force(mode: Mode, idle_secs: Option<u64>, away_minutes: u64) -> bool {
    match mode {
        Mode::Off => false,
        Mode::On => true,
        Mode::Auto => idle_secs.is_none_or(|i| i < away_minutes * 60),
    }
}

/// `"HIDIdleTime" = 4294967296` in `ioreg -c IOHIDSystem` output, nanoseconds.
pub fn parse_hid_idle(ioreg: &str) -> Option<u64> {
    let line = ioreg.lines().find(|l| l.contains("\"HIDIdleTime\""))?;
    let ns: u64 = line.split('=').nth(1)?.trim().parse().ok()?;
    Some(ns / 1_000_000_000)
}

fn hid_idle_secs() -> Option<u64> {
    parse_hid_idle(&LOCAL.idle()?)
}

/// `CPU_Speed_Limit 	= 31` in `pmset -g therm` output: percent of the rated
/// clock the machine is allowed right now.
pub fn parse_speed_limit(pmset: &str) -> Option<u32> {
    let line = pmset.lines().find(|l| l.trim_start().starts_with("CPU_Speed_Limit"))?;
    line.split('=').nth(1)?.trim().parse().ok()
}

fn speed_limit() -> Option<u32> {
    if cfg!(test) {
        return None;
    }
    parse_speed_limit(&LOCAL.thermal()?)
}

/// Three load averages out of `/proc/loadavg` (`0.5 0.4 0.3 1/200 99`) or
/// `sysctl -n vm.loadavg` (`{ 0.5 0.4 0.3 }`).
pub fn parse_loadavg(s: &str) -> Option<(f64, f64, f64)> {
    let mut n = s.split(|c: char| c.is_whitespace() || c == '{' || c == '}').filter(|w| !w.is_empty());
    Some((n.next()?.parse().ok()?, n.next()?.parse().ok()?, n.next()?.parse().ok()?))
}

fn load_average() -> Option<(f64, f64, f64)> {
    parse_loadavg(&LOCAL.load()?)
}

/// The three load averages now (`wsp-597`: a seat's death is logged with them).
pub fn load_avgs() -> Option<(f64, f64, f64)> {
    load_average()
}

/// Percent of memory free now, as `memory_pressure` gives it.
pub fn mem_free_now() -> Option<u32> {
    mem_free()
}

/// The one-minute load average now.
pub fn load1() -> Option<f64> {
    load_average().map(|l| l.0)
}

/// Is the one-minute load over the line at which new work is held (`wsp-595`:
/// a check that timed out while it was is not a verdict on the change)?
pub fn load_over_hold(one: f64, cores: usize) -> bool {
    one > HOLD_LOAD * cores as f64
}

/// Has the one-minute load fallen under the line that lets held work go?
pub fn load_released(one: f64, cores: usize) -> bool {
    one < RELEASE_LOAD * cores as f64
}

pub fn load_high(one: f64, five: f64, cores: usize) -> bool {
    one.min(five) > LOAD_FACTOR * cores as f64
}

// ---- what holds new work back (wsp-447) ----------------------------------------
//
// 2026-11-06 15:11, after a thrash at 14:20 (load 388): the 1-minute load was
// back to 4.2 and the 5-minute to 6.9, planned lists were still held, and two
// of four agent slots stood idle. The hold on new work reads the **recent**
// machine: the 1-minute load, the free memory and the clock limit. The
// 5- and 15-minute averages are history by the time a spike is over, and are
// not read for it ([`load_high`] stays the *warning*, which wants a load that
// has lasted).
//
// Each reading has two lines, the way a thermostat has: it holds at the first
// and lets go only past the second, so a load hovering at the line does not
// start and stop lists every tick. The words of what held it last are kept in
// `hold.json` in the state directory, because the CLI and the daemon are
// separate processes and a line that remembers nothing is not a hysteresis.

/// The 1-minute load, in cores, at which new work is held, and under which it
/// is let go again.
const HOLD_LOAD: f64 = LOAD_FACTOR;
const RELEASE_LOAD: f64 = 1.0;

// Memory (wsp-591). `memory_pressure`'s "free percentage" counts compressed and
// purgeable memory as free: it read 32% at 20:37 on 2026-11-09 while swap was
// 6.4 of 8 GB, 5,271 pages (20 MB) were free and the load was 734, so a gate on
// it never held anything. What says "out of memory" on macOS is paging: swap
// that is *growing*, and pages going out to it. The percentage is still read
// and shown, and gates nothing.
//
// Swap's level alone cannot gate: macOS leaves swap at 60-90% of its (growing)
// total long after the pressure has passed. This machine, on a calm evening
// (load 35), read swap 4.1 of 5.1 GB and ~1,500 pageouts a minute; the heavy
// day read swap 6.4/8 then 8.4/9.2 GB and a swap total that kept growing. So:
//
// - swap holds when it is over 60% of its total *and* grew by 100 MB a minute or
//   more; it lets go once growth is under 25 MB a minute. A full but flat swap
//   with nothing paging out is a machine that has settled, and it releases.
// - pageouts hold at 20,000 pages (4 KB) a minute, 80 MB a minute, 13 times the
//   calm figure; they let go under 5,000.
//
// Growth and the pageout rate need two samples, so the last one is kept in
// `mem.json` in the state directory (the CLI and the daemon are separate
// processes) and a rate is taken over at least [`MEM_WINDOW_SECS`] seconds.
const HOLD_SWAP_PERCENT: u32 = 60;
const HOLD_SWAP_GROWTH_MB: i64 = 100;
const RELEASE_SWAP_GROWTH_MB: i64 = 25;
const HOLD_PAGEOUTS: u32 = 20_000;
const RELEASE_PAGEOUTS: u32 = 5_000;
/// A rate is taken over at least this long; a closer call reuses the last rate.
const MEM_WINDOW_SECS: u64 = 10;
/// A sample older than this is history, not a baseline.
const MEM_STALE_SECS: u64 = 900;

/// The clock limit is let go this many points above where it held.
const RELEASE_THERMAL_ABOVE: u32 = 5;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PauseKind {
    Load,
    Memory,
    Thermal,
}

impl PauseKind {
    const ALL: [PauseKind; 3] = [PauseKind::Load, PauseKind::Memory, PauseKind::Thermal];

    fn word(self) -> &'static str {
        match self {
            PauseKind::Load => "load",
            PauseKind::Memory => "memory",
            PauseKind::Thermal => "thermal",
        }
    }
}

/// One reading that holds new work back, with the reading and the line it is
/// let go at. Load in tenths, the others in percent, so the value is `Eq` and
/// `Copy` and rides in a [`crate::waits::Wait`].
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Pause {
    pub kind: PauseKind,
    pub reading: u32,
    pub release: u32,
    /// What held, for a memory hold; `None` for the others.
    pub mem: Option<MemHold>,
}

/// The memory reading that held, in the words `wsp wip` prints.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct MemHold {
    pub swap_used_mb: u32,
    pub swap_total_mb: u32,
    /// Swap is over its line and growing.
    pub swap_growing: bool,
    pub pageouts_min: Option<u32>,
    /// The pageout rate alone holds.
    pub paging: bool,
}

fn gb(mb: u32) -> String {
    format!("{:.1}", mb as f64 / 1024.0)
}

fn thousands(n: u32) -> String {
    let s = n.to_string();
    let mut out = String::new();
    for (i, c) in s.chars().enumerate() {
        if i > 0 && (s.len() - i) % 3 == 0 {
            out.push(',');
        }
        out.push(c);
    }
    out
}

impl MemHold {
    fn text(&self) -> String {
        let mut s = format!("swap {}/{} GB", gb(self.swap_used_mb), gb(self.swap_total_mb));
        if self.swap_growing {
            s.push_str(" and growing");
        }
        if let Some(p) = self.pageouts_min {
            s.push_str(&format!(", {} pageouts/min", thousands(p)));
        }
        s
    }
}

impl Pause {
    /// The sentence `wsp worklist ls` prints: which reading held it, and when
    /// it will let go.
    pub fn text(&self) -> String {
        let tenths = |n: u32| format!("{:.1}", n as f64 / 10.0);
        match self.kind {
            PauseKind::Load => format!(
                "throttle: the 1-minute load is {}, held until it is under {}",
                tenths(self.reading),
                tenths(self.release)
            ),
            PauseKind::Memory => match self.mem {
                Some(m) => format!(
                    "throttle: memory ({}), held until swap stops growing and pageouts are under {}/min",
                    m.text(),
                    thousands(self.release)
                ),
                None => "throttle: memory".into(),
            },
            PauseKind::Thermal => format!(
                "throttle: the CPU is limited to {}% of its clock, held until it is at {}%",
                self.reading, self.release
            ),
        }
    }
}

/// The machine's recent readings, the input of [`decide_hold`].
#[derive(Clone, Copy, Debug)]
pub struct Reading {
    pub one: f64,
    pub cores: usize,
    /// Percent of memory free (`memory_pressure`); shown, gates nothing.
    pub mem_free: Option<u32>,
    /// Swap, used and total in MB; `None` where it cannot be read.
    pub swap: Option<(u32, u32)>,
    /// MB of swap gained a minute since the last sample; negative when it fell.
    pub swap_growth_mb_min: Option<i64>,
    /// Pages (4 KB) paged out a minute since the last sample.
    pub pageouts_min: Option<u32>,
    /// `CPU_Speed_Limit`; `None` where there is no `pmset`.
    pub thermal: Option<u32>,
}

/// Whether memory holds: swap over its line and growing, or pageouts over
/// theirs; once held, until growth is under the lower line and pageouts under
/// theirs. A gauge that cannot be read (no swap, no earlier sample) does not
/// hold.
fn mem_hold(was_held: bool, r: &Reading) -> Option<MemHold> {
    let (used, total) = r.swap?;
    let over = total > 0 && used as u64 * 100 >= total as u64 * HOLD_SWAP_PERCENT as u64;
    let growth_line = if was_held { RELEASE_SWAP_GROWTH_MB } else { HOLD_SWAP_GROWTH_MB };
    let swap_growing = over && r.swap_growth_mb_min.is_some_and(|g| g >= growth_line);
    let page_line = if was_held { RELEASE_PAGEOUTS } else { HOLD_PAGEOUTS };
    let paging = r.pageouts_min.is_some_and(|p| p >= page_line);
    (swap_growing || paging).then_some(MemHold {
        swap_used_mb: used,
        swap_total_mb: total,
        swap_growing,
        pageouts_min: r.pageouts_min,
        paging,
    })
}

/// Every reading that holds new work back, given the kinds that held it last
/// time (`was`). A reading that cannot be read does not hold: a machine must
/// not be held for want of a gauge.
pub fn decide_hold(was: &[PauseKind], r: &Reading, s: &Settings) -> Vec<Pause> {
    let held = |k: PauseKind| was.contains(&k);
    let tenths = |x: f64| (x * 10.0).round() as u32;
    let cores = r.cores as f64;
    let mut out = Vec::new();
    let load_line = if held(PauseKind::Load) { RELEASE_LOAD } else { HOLD_LOAD } * cores;
    if r.one > load_line {
        out.push(Pause { kind: PauseKind::Load, reading: tenths(r.one), release: tenths(RELEASE_LOAD * cores), mem: None });
    }
    if let Some(m) = mem_hold(held(PauseKind::Memory), r) {
        out.push(Pause { kind: PauseKind::Memory, reading: m.swap_used_mb, release: RELEASE_PAGEOUTS, mem: Some(m) });
    }
    if let Some(limit) = r.thermal {
        let release = (s.thermal_below + RELEASE_THERMAL_ABOVE).min(100);
        let line = if held(PauseKind::Thermal) { release } else { s.thermal_below };
        if limit < line {
            out.push(Pause { kind: PauseKind::Thermal, reading: limit, release, mem: None });
        }
    }
    out
}

/// A load hold at 14.2 on 8 cores, for tests that need one to carry.
/// The verbs this module answers to, each with the help it carries. `main` and
/// the help text are assembled from these; nothing in `main.rs` names them.
pub const VERBS: &[crate::verbs::Verb] = &[
    crate::verbs::Verb {
        names: &["throttle"],
        run: throttle,
        help: &[
            crate::verbs::Help {
                section: crate::verbs::Section::Plumbing,
                order: 120,
                text: r##"  wsp throttle [off|on|auto] [--json]
                                    how builds are held back for the person at
                                    the machine: niced, capped in jobs, and a
                                    slot for each heavy one across the fleet.
                                    `auto` (the default) holds it only while the
                                    person is at the machine; bare, it shows the
                                    mode, whether it is in force, the load and
                                    who is building. One cargo per tree is held
                                    in every mode"##,
            },
            crate::verbs::Help {
                section: crate::verbs::Section::Plumbing,
                order: 130,
                text: r##"  wsp throttle set slots=N jobs=N away=MINUTES   …the sizes (`auto` for the first two)"##,
            },
            crate::verbs::Help {
                section: crate::verbs::Section::Plumbing,
                order: 140,
                text: r##"  wsp throttle set hot-slots=N hot-jobs=N hot-below=PERCENT
                                    …what a thermally limited machine (the
                                    `pmset -g therm` CPU_Speed_Limit under
                                    PERCENT) is held to, in every mode"##,
            },
        ],
    },
];

#[cfg(any(test, feature = "test-support"))]
pub fn sample_pause() -> Pause {
    Pause { kind: PauseKind::Load, reading: 142, release: 80, mem: None }
}

fn hold_path(state: &Path) -> PathBuf {
    state.join("hold.json")
}

fn held_before(state: &Path) -> Vec<PauseKind> {
    let Some(v) = fs::read_to_string(hold_path(state)).ok().and_then(|t| serde_json::from_str::<Value>(&t).ok())
    else {
        return Vec::new();
    };
    let words: Vec<&str> = v.get("held").and_then(Value::as_array).into_iter().flatten().filter_map(Value::as_str).collect();
    PauseKind::ALL.into_iter().filter(|k| words.contains(&k.word())).collect()
}

/// Decide against what held last time and remember the answer. The state
/// directory is the machine's, so every process that asks sees one memory.
pub fn hold_step(state: &Path, r: &Reading, s: &Settings) -> Vec<Pause> {
    let was = held_before(state);
    let now = decide_hold(&was, r, s);
    let kinds: Vec<PauseKind> = now.iter().map(|h| h.kind).collect();
    if kinds != was {
        let words: Vec<&str> = kinds.iter().map(|k| k.word()).collect();
        let _ = crate::store::write_atomic(&hold_path(state), &json!({ "held": words }).to_string());
    }
    now
}

/// Percent of memory free, out of the last line of `memory_pressure`:
/// `System-wide memory free percentage: 57%`.
pub fn parse_mem_free(out: &str) -> Option<u32> {
    let line = out.lines().find(|l| l.contains("memory free percentage"))?;
    line.split(':').nth(1)?.trim().trim_end_matches('%').trim().parse().ok()
}

fn mem_free() -> Option<u32> {
    if cfg!(test) {
        return None;
    }
    #[cfg(target_os = "linux")]
    {
        return parse_meminfo_free(&std::fs::read_to_string("/proc/meminfo").ok()?);
    }
    #[cfg(not(target_os = "linux"))]
    {
    parse_mem_free(&LOCAL.memory_pressure()?)
    }
}

/// One `/proc/meminfo` field in kB.
fn meminfo_kb(meminfo: &str, key: &str) -> Option<u64> {
    meminfo.lines().find_map(|l| l.strip_prefix(key)?.strip_prefix(':')?.split_whitespace().next()?.parse().ok())
}

/// Linux: percent of memory available, `MemAvailable / MemTotal`.
pub fn parse_meminfo_free(meminfo: &str) -> Option<u32> {
    let total = meminfo_kb(meminfo, "MemTotal")?;
    let avail = meminfo_kb(meminfo, "MemAvailable")?;
    (total > 0).then(|| (avail * 100 / total).min(100) as u32)
}

/// Linux: swap in MB, `(used, total)`, out of `/proc/meminfo`. No swap is
/// `(0, 0)`, as macOS reports it.
pub fn parse_meminfo_swap(meminfo: &str) -> Option<(u32, u32)> {
    let total = meminfo_kb(meminfo, "SwapTotal")?;
    let free = meminfo_kb(meminfo, "SwapFree")?;
    Some((((total.saturating_sub(free)) / 1024) as u32, (total / 1024) as u32))
}

/// Linux: pages swapped out since boot (4 KB), `pswpout` in `/proc/vmstat`.
pub fn parse_vmstat_pageouts(vmstat: &str) -> Option<u64> {
    vmstat.lines().find_map(|l| l.strip_prefix("pswpout ")?.trim().parse().ok())
}

/// Swap in MB, `(used, total)`, out of `sysctl vm.swapusage`:
/// `vm.swapusage: total = 8192.00M  used = 6553.50M  free = 1638.50M  (encrypted)`.
pub fn parse_swapusage(out: &str) -> Option<(u32, u32)> {
    let after = |key: &str| -> Option<u32> {
        let rest = out.split(key).nth(1)?.trim_start().strip_prefix('=')?.trim_start();
        let num: String = rest.chars().take_while(|c| c.is_ascii_digit() || *c == '.').collect();
        let unit = rest[num.len()..].chars().next()?;
        let v: f64 = num.parse().ok()?;
        let mb = match unit {
            'K' => v / 1024.0,
            'M' => v,
            'G' => v * 1024.0,
            _ => return None,
        };
        Some(mb.round() as u32)
    };
    Some((after("used")?, after("total")?))
}

/// Pages paged out since boot, in 4 KB units, out of `vm_stat`:
/// `Mach Virtual Memory Statistics: (page size of 16384 bytes)` … `Pageouts:   12205611.`
pub fn parse_pageouts(out: &str) -> Option<u64> {
    let size: u64 = out
        .lines()
        .next()
        .and_then(|l| l.split("page size of").nth(1))
        .and_then(|r| r.split_whitespace().next())
        .and_then(|n| n.parse().ok())
        .unwrap_or(4096);
    let line = out.lines().find(|l| l.starts_with("Pageouts:"))?;
    let n: u64 = line.split(':').nth(1)?.trim().trim_end_matches('.').parse().ok()?;
    Some(n * size / 4096)
}

fn swap_now() -> Option<(u32, u32)> {
    if cfg!(test) {
        return None;
    }
    #[cfg(target_os = "linux")]
    {
        return parse_meminfo_swap(&std::fs::read_to_string("/proc/meminfo").ok()?);
    }
    #[cfg(not(target_os = "linux"))]
    {
    parse_swapusage(&LOCAL.swap()?)
    }
}

fn pageouts_now() -> Option<u64> {
    if cfg!(test) {
        return None;
    }
    #[cfg(target_os = "linux")]
    {
        return parse_vmstat_pageouts(&std::fs::read_to_string("/proc/vmstat").ok()?);
    }
    #[cfg(not(target_os = "linux"))]
    {
    parse_pageouts(&LOCAL.pageouts()?)
    }
}

/// The last memory sample and the rates taken from it, in `mem.json`.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
struct MemMark {
    at: u64,
    swap_used_mb: u32,
    pageouts: u64,
    growth_mb_min: Option<i64>,
    pageouts_min: Option<u32>,
}

fn mem_path(state: &Path) -> PathBuf {
    state.join("mem.json")
}

fn mem_mark_before(state: &Path) -> Option<MemMark> {
    let v: Value = serde_json::from_str(&fs::read_to_string(mem_path(state)).ok()?).ok()?;
    Some(MemMark {
        at: v.get("at")?.as_u64()?,
        swap_used_mb: v.get("swap_used_mb")?.as_u64()? as u32,
        pageouts: v.get("pageouts")?.as_u64()?,
        growth_mb_min: v.get("growth_mb_min").and_then(Value::as_i64),
        pageouts_min: v.get("pageouts_min").and_then(Value::as_u64).map(|n| n as u32),
    })
}

/// Take the rates for a sample at `now` (seconds) against the last mark, and
/// remember it. Closer than [`MEM_WINDOW_SECS`] to the last, the last rates
/// stand and the baseline stays, so a caller that asks every second reads the
/// rate over a window, not the noise of one tick. A mark older than
/// [`MEM_STALE_SECS`], or counters that went backwards (a reboot), give no rate.
/// Returns `(swap growth MB/min, pageouts/min)`.
fn mem_rates(state: &Path, now: u64, swap_used_mb: u32, pageouts: u64) -> (Option<i64>, Option<u32>) {
    let prev = mem_mark_before(state);
    if let Some(p) = prev {
        let dt = now.saturating_sub(p.at);
        if now >= p.at && dt < MEM_WINDOW_SECS {
            return (p.growth_mb_min, p.pageouts_min);
        }
    }
    let rates = prev
        .filter(|p| now > p.at && now - p.at <= MEM_STALE_SECS && pageouts >= p.pageouts)
        .map(|p| {
            let dt = (now - p.at) as f64;
            let growth = ((swap_used_mb as f64 - p.swap_used_mb as f64) * 60.0 / dt).round() as i64;
            let paged = ((pageouts - p.pageouts) as f64 * 60.0 / dt).round().min(u32::MAX as f64) as u32;
            (Some(growth), Some(paged))
        })
        .unwrap_or((None, None));
    let mark = json!({
        "at": now,
        "swap_used_mb": swap_used_mb,
        "pageouts": pageouts,
        "growth_mb_min": rates.0,
        "pageouts_min": rates.1,
    });
    let _ = crate::store::write_atomic(&mem_path(state), &mark.to_string());
    rates
}

fn unix_now() -> u64 {
    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0, |d| d.as_secs())
}

/// An unreadable load reads as none, so the memory and the clock still count.
fn reading_now(state: &Path) -> Reading {
    let swap = swap_now();
    let (growth, paged) = match (swap, pageouts_now()) {
        (Some((used, _)), Some(po)) => mem_rates(state, unix_now(), used, po),
        _ => (None, None),
    };
    Reading {
        one: load_average().map_or(0.0, |l| l.0),
        cores: cores(),
        mem_free: mem_free(),
        swap,
        swap_growth_mb_min: growth,
        pageouts_min: paged,
        thermal: speed_limit(),
    }
}

/// How many times the readiness wait of a spawn is stretched at this load
/// (`wsp-335`): once per core's worth of load queued, from 1 to 4. A
/// `compound-956` agent never became ready inside sixty seconds at load about
/// 60 on 8 cores; the same agent is up in twenty on a quiet machine, so the
/// wait grows with the load rather than for everyone. The cap is four minutes:
/// past that the start is better ended and tried again on the backoff.
pub fn readiness_scale(one: f64, cores: usize) -> u32 {
    ((one / cores.max(1) as f64).ceil() as u32).clamp(1, 4)
}

/// [`readiness_scale`] for the machine as it is now. 1 when the load cannot be
/// read, and under test, where the clock is a dial and a load reading is noise.
pub fn readiness_scale_now() -> u32 {
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
pub enum Weight {
    /// Compiles and finishes: holds the tree and takes a slot.
    Heavy,
    /// Compiles and may then stay up (`cargo run`): niced and capped like a
    /// heavy build, but holds nothing, so the compile it contains is not the
    /// unthrottled one and a long run does not hold the tree.
    Runs,
    Light,
}

pub fn weight(tool: &str, args: &[String]) -> Weight {
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
    // Could not ask reads as not alive, as it did before the Host.
    crate::host::LOCAL.probe(pid).unwrap_or(false)
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
pub fn tree_of(cwd: &Path) -> PathBuf {
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
pub struct Hold {
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
pub struct Ctx<'a> {
    pub state: &'a Path,
    pub cwd: PathBuf,
    pub path_var: String,
    pub task: String,
    pub cores: usize,
    pub idle: &'a dyn Fn() -> Option<u64>,
    /// `CPU_Speed_Limit` now, read at each decision: it moves with the build.
    pub thermal: &'a dyn Fn() -> Option<u32>,
    pub alive: &'a dyn Fn(u32) -> bool,
    pub say: &'a dyn Fn(&str),
    pub poll: Duration,
    /// Set for a build already inside a hold.
    pub held: bool,
    /// The variable already in the build's environment, by name.
    pub env: &'a dyn Fn(&str) -> Option<String>,
    /// A line for the row's log, by row (`wsp-538`: the reason a member gave for
    /// a full suite).
    pub log: &'a dyn Fn(&str, &str),
    /// The row's body, by row: its `## Checks` and the barrier's failing tests,
    /// named in the refusal so the narrow path is the obvious one.
    pub body: &'a dyn Fn(&str) -> String,
}

/// What a member says to run a full suite anyway (`wsp-538`).
pub const FULL_SUITE_WHY: &str = "WSP_FULL_SUITE";

/// What the seat is, in `WSP_SEAT_KIND`: `member`, `verifier` or `check`.
/// Only a member is held to its row's own checks.
pub const SEAT_KIND: &str = "WSP_SEAT_KIND";

/// The row's log line for a full suite a member ran with a reason.
pub const FULL_SUITE_LOGGED: &str = "full suite run in this seat:";

/// Whether a command is a full-workspace run a member is asked to confirm:
/// `cargo test` / `cargo nextest run` with no filter (see [`is_full_suite`]), or
/// the full `npm run check` of a React tree.
pub fn is_gated_suite(tool: &str, args: &[String]) -> bool {
    if tool == "npm" {
        return args.len() == 2 && args[0] == "run" && args[1] == "check";
    }
    is_full_suite(tool, args)
}

/// The full-workspace command a piece of row text requires, if it does: a
/// backticked span (or a line) that is `cargo test` / `cargo nextest run` with no
/// filter, or the full `npm run check` (`wsp-538`). A done-when that says so
/// sends the member to the one run the barrier owns.
pub fn asks_full_suite(text: &str) -> Option<String> {
    let mut cands: Vec<&str> = text.split('`').skip(1).step_by(2).collect();
    cands.extend(text.lines().map(|l| l.trim().trim_start_matches("$ ").trim_start_matches("verify").trim()));
    cands.into_iter().find_map(|c| {
        let words: Vec<String> = c.split_whitespace().map(|w| w.trim_end_matches(['.', ',', ';']).to_string()).collect();
        let (tool, rest) = words.split_first()?;
        let tool = tool.as_str();
        // A sentence that starts with a command's name is not a command: it
        // has to read as the command, whole (`cargo test` and its flags).
        let rest: Vec<String> = rest.iter().take_while(|w| w.starts_with('-') || matches!(w.as_str(), "run" | "nextest" | "test" | "t" | "check") || w.starts_with('+')).cloned().collect();
        let full = rest.len() + 1 == words.len() && is_gated_suite(tool, &rest);
        full.then(|| c.to_string())
    })
}

/// Why a row may not require a full-workspace run of its member.
pub fn full_suite_ask_refusal(found: &str) -> String {
    format!(
        "wsp: this asks the member to run `{found}`, the whole suite. The barrier runs it once per group; a row's done-when names the failing test and the scoped check instead (`cargo nextest run -E 'test(=<test>)'` or `wsp check <row>`). A person may still write one."
    )
}

/// The `## Checks` lines of a row, as written.
fn checks_of(body: &str) -> Vec<String> {
    let mut in_checks = false;
    let mut out = Vec::new();
    for l in body.lines() {
        if let Some(h) = l.strip_prefix("## ") {
            in_checks = h.trim() == "Checks";
            continue;
        }
        if in_checks && !l.trim().is_empty() {
            out.push(l.trim().to_string());
        }
    }
    out
}

/// The tests a nextest run in the row's body reports failing: `FAIL [ 0.2s] crate path::name`.
fn failing_tests(body: &str) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    for l in body.lines() {
        let l = l.trim().trim_start_matches("- ").trim();
        let Some(at) = l.find("FAIL [") else { continue };
        let Some(close) = l[at..].find(']') else { continue };
        let words: Vec<&str> = l[at + close + 1..].split_whitespace().collect();
        if let Some(name) = words.last().filter(|n| n.contains("::") || words.len() == 1) {
            let name = name.to_string();
            if !out.contains(&name) {
                out.push(name);
            }
        }
    }
    out
}

/// What a member is told when it asks for the whole suite without a reason.
pub fn full_suite_refusal(body: &str) -> String {
    let mut m = format!(
        "wsp: refused. The barrier runs the full suite once per group; run your row's checks (`wsp check <row>`). If this change needs the whole suite (a shared module, a fixture every test reads, a build-wide setting), run it again with `{FULL_SUITE_WHY}=\"<why>\"`."
    );
    let checks = checks_of(body);
    if !checks.is_empty() {
        m.push_str("\nYour row's checks:");
        for c in checks.iter().take(8) {
            m.push_str(&format!("\n  {c}"));
        }
    }
    let failing = failing_tests(body);
    if !failing.is_empty() {
        m.push_str("\nA test the barrier saw fail, run alone:");
        for t in failing.iter().take(5) {
            m.push_str(&format!("\n  cargo nextest run -E 'test(={t})'"));
        }
    }
    m
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
pub fn real_tool(tool: &str, path: &str, shims: &Path) -> Option<PathBuf> {
    use std::os::unix::fs::PermissionsExt;
    path.split(':')
        .filter(|d| !d.is_empty() && Path::new(d) != shims)
        .map(|d| Path::new(d).join(tool))
        .find(|p| fs::metadata(p).is_ok_and(|m| m.is_file() && m.permissions().mode() & 0o111 != 0) && !is_shim(p))
}

/// Whether `cargo test` is the whole suite: no filter, no target or package
/// narrowing it. Any bare word after the subcommand is a filter (or a flag's
/// value, which errs toward calling the run narrow, never the other way).
pub fn is_full_suite(tool: &str, args: &[String]) -> bool {
    if tool != "cargo" {
        return false;
    }
    // `cargo nextest run` is the same suite under a per-test timeout (wsp-440).
    let nextest = matches!(subcommand(args), Some("nextest"));
    if !nextest && !matches!(subcommand(args), Some("test" | "t")) {
        return false;
    }
    const NARROW: &[&str] = &["--lib", "--bins", "--bin", "--test", "--tests", "--doc", "--example", "--examples", "--bench", "--benches", "-p", "--package"];
    let at = args.iter().position(|a| !a.starts_with('+') && !a.starts_with('-')).unwrap_or(0);
    let mut rest = &args[at + 1..];
    if nextest {
        if rest.first().map(String::as_str) != Some("run") {
            return false;
        }
        rest = &rest[1..];
    }
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
    if let Some(seat) = (ctx.env)(crate::base::seat::SEAT_ENV) {
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
pub fn full_suite_runs(state: &Path, task: &str) -> u64 {
    full_suite_record(state, task).get("runs").and_then(Value::as_u64).unwrap_or(0)
}

/// The seats that have started a full suite.
pub fn full_suite_seats(state: &Path) -> std::collections::BTreeSet<String> {
    let Ok(rd) = fs::read_dir(full_suite_dir(state)) else { return Default::default() };
    rd.flatten()
        .filter_map(|e| fs::read_to_string(e.path()).ok())
        .filter_map(|t| serde_json::from_str::<Value>(&t).ok())
        .flat_map(|v| v.get("seats").and_then(Value::as_object).map(|o| o.keys().cloned().collect::<Vec<_>>()).unwrap_or_default())
        .collect()
}

pub fn run_in(ctx: &Ctx, tool: &str, args: &[String]) -> i32 {
    let settings = Settings::load(ctx.state);
    let shims = shim_dir(ctx.state);
    let Some(real) = real_tool(tool, &ctx.path_var, &shims) else {
        eprintln!("wsp: no `{tool}` on PATH past wsp's own shim");
        return 127;
    };
    if !ctx.held && !ctx.task.is_empty() && is_gated_suite(tool, args) {
        // A member confirms, with a reason, before the whole suite (`wsp-538`);
        // a verifier is noted as before, and a check or the barrier has no task.
        if (ctx.env)(SEAT_KIND).as_deref() == Some("member") {
            match (ctx.env)(FULL_SUITE_WHY).map(|w| w.trim().to_string()).filter(|w| !w.is_empty()) {
                Some(why) => (ctx.log)(&ctx.task, &format!("{FULL_SUITE_LOGGED} {why}")),
                None => {
                    (ctx.say)(&full_suite_refusal(&(ctx.body)(&ctx.task)));
                    return 2;
                }
            }
        }
        if tool == "cargo" {
            note_full_suite(ctx);
        }
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
    // A thermally limited machine is throughput, not politeness (`wsp-347`):
    // the slots and the job cap hold in every mode, `off` included, and the
    // nice stays the person's.
    let hot = || settings.is_hot((ctx.thermal)());
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
        if force() || hot() {
            let mut grab = || {
                // Read at each try: a wait that began cool ends hot.
                let n = settings.slots_at(ctx.cores, hot());
                let mut seen = Value::Null;
                for i in 0..n {
                    match take(&slot_path(ctx.state, i), &rec, ctx.alive) {
                        Taken::Yours(h) => return Taken::Yours(h),
                        Taken::HeldBy(v) => seen = v,
                    }
                }
                Taken::HeldBy(seen)
            };
            _holds.push(wait_for(ctx, "a build slot", &mut grab, Some(&|| !force() && !hot())));
        }
        cmd.env(HELD, tree.display().to_string());
    }
    let mut niced = None;
    let limited = hot();
    if force() || limited {
        if force() {
            niced = Some(NICE);
        }
        if let Some(var) = settings.tools.get(tool) {
            let wanted = settings.jobs_at(ctx.cores, limited);
            let wanted = if force() && limited { wanted.min(settings.job_cap(ctx.cores)) } else { wanted };
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
    wait_loop(ctx.say, ctx.poll, what, attempt, lifted)
}

fn wait_loop(
    say: &dyn Fn(&str),
    poll: Duration,
    what: &str,
    attempt: &mut dyn FnMut() -> Taken,
    lifted: Option<&dyn Fn() -> bool>,
) -> Hold {
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
            say(&format!(
                "wsp: waiting for {what} — held by {by}. Queued, not failed; this is wsp's build throttle (`wsp throttle` shows it, `wsp throttle off` lifts the slots, not the one-build-per-tree hold)"
            ));
            next_say = waited + Duration::from_secs(60);
        }
        std::thread::sleep(poll);
        waited += poll;
        polls += 1;
        if polls % 10 == 0 && lifted.is_some_and(|l| l()) {
            return Hold { path: PathBuf::new() };
        }
    }
}

/// What a check, a verifier's build or a barrier run reads to take a slot
/// (`wsp-668`), so it can be made against a fake.
pub struct CheckGate<'a> {
    pub state: &'a Path,
    pub cores: usize,
    pub task: String,
    pub cmd: String,
    pub dir: PathBuf,
    /// `CPU_Speed_Limit` now, read at each try: it moves with the work.
    pub thermal: &'a dyn Fn() -> Option<u32>,
    pub alive: &'a dyn Fn(u32) -> bool,
    pub say: &'a dyn Fn(&str),
    pub poll: Duration,
}

fn check_path(state: &Path, n: usize) -> PathBuf {
    builds_dir(state).join(format!("check-{n}"))
}

fn waiting_path(state: &Path) -> PathBuf {
    builds_dir(state).join(format!("waiting-{}-{}", std::process::id(), WAITS.fetch_add(1, Ordering::SeqCst)))
}

static WAITS: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);

/// The slot for a check step now: in the process's own state directory, read
/// from the machine. Not under test without a `WSP_STATE` of its own, where
/// the live directory is not the test's to hold.
pub fn check_slot_now(dir: &Path, command: &str) -> Hold {
    let state = match std::env::var_os("WSP_STATE") {
        Some(v) => PathBuf::from(v),
        None if cfg!(test) => return Hold { path: PathBuf::new() },
        None => Store::default_state(),
    };
    let say = |m: &str| eprintln!("{m}");
    take_check_slot(&CheckGate {
        state: &state,
        cores: cores(),
        task: std::env::var("WSP_TASK").unwrap_or_default(),
        cmd: command.chars().take(80).collect(),
        dir: dir.to_path_buf(),
        thermal: &speed_limit,
        alive: &alive,
        say: &say,
        poll: Duration::from_secs(1),
    })
}

/// A check slot, waited for when none is free, released when the hold drops.
/// The slots are the machine's, not a tree's: a check in any tree takes one,
/// and they are not the build slots a `cargo` takes inside it, so a check that
/// runs `cargo test` does not wait on itself. `off` lifts the cap on a cool
/// machine; a limited clock holds it in every mode.
pub fn take_check_slot(g: &CheckGate) -> Hold {
    let settings = Settings::load(g.state);
    let hot = || settings.is_hot((g.thermal)());
    if settings.mode == Mode::Off && !hot() {
        return Hold { path: PathBuf::new() };
    }
    let rec = record(&g.task, &g.cmd, &[], &tree_of(&g.dir));
    let mut grab = || {
        // Read at each try: a wait that began cool ends hot.
        let n = settings.check_slots_at(g.cores, hot());
        let mut seen = Value::Null;
        for i in 0..n {
            match take(&check_path(g.state, i), &rec, g.alive) {
                Taken::Yours(h) => return Taken::Yours(h),
                Taken::HeldBy(v) => seen = v,
            }
        }
        Taken::HeldBy(seen)
    };
    if let Taken::Yours(h) = grab() {
        return h;
    }
    // Waiting is on the record too, so `wsp wip` names what queues.
    let _waiting = take(&waiting_path(g.state), &rec, g.alive);
    wait_loop(g.say, g.poll, "a check slot", &mut grab, Some(&|| settings.mode == Mode::Off && !hot()))
}

fn finish(mut cmd: Command, nice: Option<i32>) -> i32 {
    use std::os::unix::process::ExitStatusExt;
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
pub fn run_raw(argv: &[String]) -> i32 {
    let Some((tool, args)) = argv.split_first() else {
        eprintln!("usage: wsp throttle run <tool> [args…]");
        return 2;
    };
    let store = Store::open();
    let idle = hid_idle_secs;
    let thermal = speed_limit;
    let say = |m: &str| eprintln!("{m}");
    let env = |k: &str| std::env::var(k).ok().filter(|v| !v.is_empty());
    let ctx = Ctx {
        state: &store.state,
        cwd: std::env::current_dir().unwrap_or_default(),
        path_var: std::env::var("PATH").unwrap_or_default(),
        task: std::env::var("WSP_TASK").unwrap_or_default(),
        cores: cores(),
        idle: &idle,
        thermal: &thermal,
        alive: &alive,
        say: &say,
        poll: Duration::from_secs(1),
        held: std::env::var_os(HELD).is_some_and(|v| !v.is_empty()),
        env: &env,
        log: &|task, line| {
            if let Some(mut t) = store.find_task(task) {
                t.log(line);
                t.touch();
                let _ = store.save_task(&t);
            }
        },
        body: &|task| store.find_task(task).map(|t| t.body).unwrap_or_default(),
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
pub fn ensure_shims(state: &Path, settings: &Settings) -> PathBuf {
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
pub fn seat_env() -> BTreeMap<String, String> {
    let state = match std::env::var_os("WSP_STATE") {
        Some(v) => PathBuf::from(v),
        // A test that has not isolated itself must not write shims into the
        // live state directory.
        None if cfg!(test) => return BTreeMap::new(),
        None => Store::default_state(),
    };
    seat_env_in(&state, &std::env::var("PATH").unwrap_or_default())
}

pub fn seat_env_in(state: &Path, path: &str) -> BTreeMap<String, String> {
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

pub struct Build {
    pub holder: Value,
    pub slot: Option<usize>,
}

pub struct View {
    pub settings: Settings,
    pub idle: Option<u64>,
    pub in_force: bool,
    pub cores: usize,
    pub load: Option<(f64, f64, f64)>,
    /// `CPU_Speed_Limit`, percent of the rated clock; `None` where there is no
    /// `pmset`.
    pub thermal: Option<u32>,
    pub builds: Vec<Build>,
    /// The check slots in use and the checks queued for one (`wsp-668`).
    pub checks: Checks,
    pub shims: PathBuf,
}

/// Who holds the check slots, by slot, and who waits for one.
#[derive(Clone, Debug, Default)]
pub struct Checks {
    pub running: Vec<(usize, Value)>,
    pub waiting: Vec<Value>,
}

impl View {
    pub fn read(state: &Path) -> View {
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
            thermal: speed_limit(),
            builds: builds(state, &alive),
            checks: checks(state, &alive),
            shims: shim_dir(state),
        }
    }

    /// [`View::read`] with the machine's facts handed in ([`Given`]): the
    /// person's idle time is not read, so `auto` reads as in force.
    pub fn given(state: &Path, g: Given) -> View {
        View::of_beat(state, g, None, &alive)
    }

    /// [`View::given`] with the person's idle time handed in as well (a beat's
    /// `hid_idle_secs`, read under `auto` only, as [`View::read`] reads it),
    /// and each build holder's liveness asked of `alive`: for `wip` through
    /// the API, which reads the machine from the manager's beat and runs no
    /// child (wsp-076).
    pub fn of_beat(state: &Path, g: Given, idle_secs: Option<u64>, alive: &dyn Fn(u32) -> bool) -> View {
        let settings = Settings::load(state);
        let idle = match settings.mode {
            Mode::Auto => idle_secs,
            _ => None,
        };
        let in_force = in_force(settings.mode, idle, settings.away_minutes);
        View {
            settings,
            idle,
            in_force,
            cores: g.cores.max(1),
            load: Some((g.load1, g.load1, g.load1)),
            thermal: g.speed_limit,
            builds: builds(state, alive),
            shims: shim_dir(state),
        }
    }

    pub fn load_warning(&self) -> bool {
        self.load.is_some_and(|(one, five, _)| load_high(one, five, self.cores))
    }

    /// Thermally limited now (`wsp-347`): in every mode, the slots and jobs
    /// are the hot ones.
    pub fn hot(&self) -> bool {
        self.settings.is_hot(self.thermal)
    }

    fn thermal_sentence(&self, agent_cap: Option<usize>) -> String {
        let s = &self.settings;
        let limit = self.thermal.unwrap_or(100);
        let mut out = format!(
            "the CPU is thermally limited to {limit}% of its clock (under {}%), so builds are held to {} at once and {} job{} each in every mode — more parallel builds add heat and finish fewer",
            s.thermal_below,
            s.slots_at(self.cores, true),
            s.jobs_at(self.cores, true),
            if s.jobs_at(self.cores, true) == 1 { "" } else { "s" },
        );
        let want = s.hot_agents(self.cores);
        if agent_cap.is_none_or(|c| c > want) {
            out.push_str(&format!("; fewer agents at once would help: `wsp machine set seat agents={want}`"));
        }
        out
    }

    /// Whatever is wrong with the machine worth a warning line: the load, the
    /// clock, or both.
    pub fn warning(&self, agent_cap: Option<usize>) -> Option<String> {
        let parts: Vec<String> = [
            self.load_warning().then(|| self.load_sentence()),
            self.hot().then(|| self.thermal_sentence(agent_cap)),
        ]
        .into_iter()
        .flatten()
        .collect();
        (!parts.is_empty()).then(|| parts.join("; "))
    }

    fn thermal_cell(&self) -> Option<String> {
        self.thermal.map(|l| match self.hot() {
            true => format!("cpu clock {l}% — thermally limited"),
            false => format!("cpu clock {l}%"),
        })
    }

    fn force_line(&self) -> String {
        let s = &self.settings;
        match (s.mode, self.in_force) {
            (Mode::Off, _) if self.hot() => "off — builds run at full priority, but the CPU is thermally limited, so slots and jobs hold".into(),
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

    pub fn render(&self) -> String {
        let s = &self.settings;
        let mut out = vec![format!("throttle: {}", self.force_line())];
        out.push(format!(
            "  when in force: builds niced +{NICE}, {} job{} each, {} at once machine-wide ({} cores)",
            s.job_cap(self.cores),
            if s.job_cap(self.cores) == 1 { "" } else { "s" },
            s.slot_count(self.cores),
            self.cores
        ));
        if self.hot() {
            out.push(format!(
                "  thermally limited, so in every mode: {} at once machine-wide, {} job{} each",
                s.slots_at(self.cores, true),
                s.jobs_at(self.cores, true),
                if s.jobs_at(self.cores, true) == 1 { "" } else { "s" },
            ));
        }
        let hot = self.hot();
        out.push(format!(
            "  checks: {} at once machine-wide{}",
            s.check_slots_at(self.cores, hot),
            if hot { " (the clock is limited)" } else { "" }
        ));
        out.push("  always: one cargo build per tree".into());
        out.push(format!("  shims: {} (first on every spawned seat's PATH)", util::contract(&self.shims)));
        if let Some((a, b, c)) = self.load {
            out.push(format!("load: {a:.1} {b:.1} {c:.1} on {} cores", self.cores));
        }
        if let Some(c) = self.thermal_cell() {
            out.push(format!("thermal: {c} (limited under {}%)", s.thermal_below));
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
        for (n, h) in &self.checks.running {
            out.push(format!("  check slot {}/{}  {}", n + 1, s.check_slots_at(self.cores, hot), who(h)));
        }
        for h in &self.checks.waiting {
            out.push(format!("  waiting for a check slot  {}", who(h)));
        }
        out.join("\n")
    }

    /// The one line `wsp wip` and `wsp watch --status` carry in every mode: the
    /// mode, whether it holds this minute, and who is building.
    pub fn status_line(&self) -> String {
        let building = match self.builds.is_empty() {
            true => String::new(),
            false => format!(
                " · building: {}",
                self.builds.iter().map(|b| who(&b.holder)).collect::<Vec<_>>().join("; ")
            ),
        };
        let thermal = self.thermal_cell().map_or(String::new(), |c| format!(" · {c}"));
        let slots = self.settings.check_slots_at(self.cores, self.hot());
        let mut checks = format!(" · check slots {}/{slots}", self.checks.running.len());
        if !self.checks.waiting.is_empty() {
            checks.push_str(&format!(
                ", waiting: {}",
                self.checks.waiting.iter().map(who).collect::<Vec<_>>().join("; ")
            ));
        }
        format!("throttle: {}{thermal}{checks}{building}", self.force_line())
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

fn checks(state: &Path, alive: &dyn Fn(u32) -> bool) -> Checks {
    let mut out = Checks::default();
    for e in fs::read_dir(builds_dir(state)).into_iter().flatten().flatten() {
        let name = e.file_name().to_string_lossy().to_string();
        let Some(v) = fs::read_to_string(e.path()).ok().and_then(|t| serde_json::from_str::<Value>(&t).ok()) else { continue };
        if !alive(holder_pid(&v)) {
            continue;
        }
        if let Some(n) = name.strip_prefix("check-").and_then(|n| n.parse::<usize>().ok()) {
            out.running.push((n, v));
        } else if name.starts_with("waiting-") && !name.ends_with(".reclaim") {
            out.waiting.push(v);
        }
    }
    out.running.sort_by_key(|(n, _)| *n);
    out.waiting.sort_by_key(|v| v.get("since").and_then(Value::as_i64).unwrap_or(0));
    out
}

/// The warning alone, for `wsp wip`: absent while the machine is fine.
pub fn loud(store: &Store) -> Option<String> {
    View::read(&store.state).warning(store.seat_cap())
}

/// What holds new work back now, the first reading, for what waits on the
/// machine (`wsp-383`). Decided on the recent load, memory and clock with
/// hysteresis (`wsp-447`), not on the warning [`loud`] words. Never under
/// test, where a reading is noise; [`hold_step`] is what the tests drive.
pub fn holding_back(store: &Store) -> Option<Pause> {
    if cfg!(test) {
        return None;
    }
    let s = Settings::load(&store.state);
    hold_step(&store.state, &reading_now(&store.state), &s).into_iter().next()
}

/// The memory readings as `wsp throttle --json` carries them, and which, if
/// any, is holding. Blank under test, like [`holding_back`].
pub fn memory_json(store: &Store) -> Value {
    if cfg!(test) {
        return Value::Null;
    }
    let s = Settings::load(&store.state);
    let r = reading_now(&store.state);
    let held = hold_step(&store.state, &r, &s).into_iter().find(|h| h.kind == PauseKind::Memory);
    memory_json_of(&r, held)
}

fn memory_json_of(r: &Reading, held: Option<Pause>) -> Value {
    json!({
        "free_percent": r.mem_free,
        "swap_used_mb": r.swap.map(|s| s.0),
        "swap_total_mb": r.swap.map(|s| s.1),
        "swap_growth_mb_per_min": r.swap_growth_mb_min,
        "pageouts_per_min": r.pageouts_min,
        "held": held.is_some(),
        "held_text": held.map(|h| h.text()),
    })
}

/// [`View::status_line`], for the surfaces that are not `wsp throttle`.
pub fn status_line(store: &Store) -> String {
    View::read(&store.state).status_line()
}

pub fn health(store: &Store, notes: &mut Vec<String>) {
    let v = View::read(&store.state);
    notes.push(format!("build throttle {}", v.force_line()));
    if let Some(w) = v.warning(store.seat_cap()) {
        notes.push(format!("⚠ {w}"));
    }
}

static WARNED: AtomicBool = AtomicBool::new(false);

/// The daemon's half: drop holds whose process is gone, and log the load
/// warning when it starts and when it clears. Not under test; [`reap`] and
/// [`View`] are.
pub fn tidy(store: &Store) {
    if cfg!(test) {
        return;
    }
    reap(&store.state, &alive);
    warn(store, &View::read(&store.state));
}

/// The facts a manager's beat carries, in place of this process's own
/// `sysctl` and `pmset` (`docs/agent-manager.md` 4.3; wsp-028).
#[derive(Clone, Copy, Debug)]
pub struct Given {
    /// The one-minute load. The beat carries no five-minute one, so the
    /// warning's "for a while" is the one-minute load alone here.
    pub load1: f64,
    pub cores: usize,
    /// The thermal state as a `CPU_Speed_Limit`: the top of its band.
    pub speed_limit: Option<u32>,
}

/// [`tidy`] over `given` rather than a reading of its own: no `sysctl`, no
/// `pmset`, no `ioreg`. Under test as well, because nothing here probes.
pub fn tidy_given(store: &Store, given: Option<Given>) {
    if !cfg!(test) {
        reap(&store.state, &alive);
    }
    // Nothing folded yet is no reading, and no warning edge either way.
    let Some(g) = given else { return };
    warn(store, &View::given(&store.state, g));
}

fn warn(store: &Store, v: &View) {
    let warning = v.warning(store.seat_cap());
    if warning.is_some() != WARNED.swap(warning.is_some(), Ordering::SeqCst) {
        match warning {
            Some(w) => crate::store::log_line(store, &w),
            None => crate::store::log_line(store, "load and clock are back under the warning line"),
        }
    }
}

pub fn reap(state: &Path, alive: &dyn Fn(u32) -> bool) -> usize {
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

pub fn throttle(store: &Store, args: &Args) -> i32 {
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
                    eprintln!("wsp: `{p}` is not k=v — slots=N jobs=N checks=N hot-slots=N hot-jobs=N hot-below=PERCENT away=MINUTES; `auto` for the counts");
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
                    ("hot-slots", "auto") => s.hot_slots = None,
                    ("hot-jobs", "auto") => s.hot_jobs = None,
                    ("hot-slots", _) => match n() {
                        Some(n) => s.hot_slots = Some(n),
                        None => return bad(k, v),
                    },
                    ("hot-jobs", _) => match n() {
                        Some(n) => s.hot_jobs = Some(n),
                        None => return bad(k, v),
                    },
                    ("checks", "auto") => s.checks = None,
                    ("checks", _) => match n() {
                        Some(n) => s.checks = Some(n),
                        None => return bad(k, v),
                    },
                    ("hot-below", _) => match v.parse::<u32>() {
                        Ok(n) if n <= 100 => s.thermal_below = n,
                        _ => return bad(k, v),
                    },
                    ("away", _) => match v.parse::<u64>() {
                        Ok(n) => s.away_minutes = n,
                        Err(_) => return bad(k, v),
                    },
                    _ => {
                        eprintln!("wsp: no throttle setting `{k}` — slots, jobs, checks, away, hot-slots, hot-jobs, hot-below");
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
            eprintln!("usage: wsp throttle [off|on|auto | set slots=N jobs=N hot-slots=N hot-jobs=N hot-below=PERCENT away=MINUTES]   (bare: what it is doing now)");
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
                "memory": memory_json(store),
                "cpu_speed_limit": v.thermal,
                "thermal_below": v.settings.thermal_below,
                "thermally_limited": v.hot(),
                "hot_slots": v.settings.slots_at(v.cores, true),
                "hot_jobs": v.settings.jobs_at(v.cores, true),
                "suggested_agents": v.hot().then(|| v.settings.hot_agents(v.cores)),
                "agents": store.seat_cap(),
                "builds": v.builds.iter().map(|b| json!({"holder": b.holder, "slot": b.slot})).collect::<Vec<_>>(),
            }))
            .unwrap_or_default()
        ),
        false => {
            println!("{}", v.render());
            match holding_back(store) {
                Some(h) => println!("new work: held — {}", h.text().trim_start_matches("throttle: ")),
                None => println!("new work: not held (1-minute load, memory and clock are all fine)"),
            }
            let cap = store.seat_cap();
            let want = v.settings.hot_agents(v.cores);
            if v.hot() && cap.is_none_or(|c| c > want) {
                println!(
                    "agents: {} — the machine is thermally limited; fewer agents at once would help: wsp machine set seat agents={want}",
                    cap.map_or("no cap".to_string(), |c| format!("capped at {c}"))
                );
            }
        }
    }
    0
}

fn bad(k: &str, v: &str) -> i32 {
    eprintln!("wsp: `{k}={v}` is not a number worth setting");
    2
}

#[cfg(test)]
mod tests {
    #[test]
    fn linux_proc_readers() {
        let mi = "MemTotal:       16000000 kB\nMemAvailable:    4000000 kB\nSwapTotal:       2097152 kB\nSwapFree:        1048576 kB\n";
        assert_eq!(parse_meminfo_free(mi), Some(25));
        assert_eq!(parse_meminfo_swap(mi), Some((1024, 2048)));
        assert_eq!(parse_vmstat_pageouts("pswpin 3\npswpout 4242\n"), Some(4242));
        assert_eq!(parse_meminfo_free(""), None);
    }

    use super::*;
    use std::cell::RefCell;

    fn s(w: &[&str]) -> Vec<String> {
        w.iter().map(|x| x.to_string()).collect()
    }

    #[test]
    fn a_spike_then_recovery_releases_on_the_one_minute_load_not_the_history() {
        // wsp-447: 8 cores. The 5- and 15-minute averages are not an input at
        // all, so a series that still carries a 37 behind it releases as soon
        // as the 1-minute load is under the release line.
        let t = crate::util::isolated("throttle-hold-series");
        let st = Settings::default();
        let at = |one: f64| Reading { one, ..calm() };
        // (seconds since the spike, 1-minute load, held?)
        let series: [(u32, f64, bool); 9] = [
            (0, 4.0, false),
            (30, 388.0, true),
            (90, 120.0, true),
            (150, 30.0, true),
            (210, 11.0, true), // under the hold line, over the release line: still held
            (270, 8.5, true),
            (330, 7.9, false), // recovered: released
            (390, 9.0, false), // between the lines again: not held again, no flap
            (450, 4.2, false),
        ];
        for (secs, one, want) in series {
            let got = hold_step(&t.state(), &at(one), &st);
            assert_eq!(!got.is_empty(), want, "at {secs}s, load {one}: {got:?}");
        }
        // The hold names the reading, so `wsp worklist ls` can say it.
        let h = hold_step(&t.state(), &at(14.2), &st);
        assert_eq!(h.len(), 1);
        assert_eq!(h[0].text(), "throttle: the 1-minute load is 14.2, held until it is under 8.0");
    }

    /// A machine with nothing wrong: load 1 on 8 cores, a full clock, swap
    /// 80% used but flat, nothing paging, and `memory_pressure` happy.
    fn calm() -> Reading {
        Reading {
            one: 1.0,
            cores: 8,
            mem_free: Some(57),
            swap: Some((4096, 5120)),
            swap_growth_mb_min: Some(0),
            pageouts_min: Some(1_500),
            thermal: Some(100),
        }
    }

    #[test]
    fn swap_over_the_line_and_growing_holds_and_flat_with_no_paging_releases() {
        let t = crate::util::isolated("throttle-hold-swap");
        let st = Settings::default();
        let r = |used: u32, growth: Option<i64>, po: Option<u32>| Reading {
            swap: Some((used, 9216)),
            swap_growth_mb_min: growth,
            pageouts_min: po,
            ..calm()
        };
        // The heavy day: 8.4 of 9.2 GB and 1,200 pageouts a minute, growing.
        let h = hold_step(&t.state(), &r(8602, Some(300), Some(1_200)), &st);
        assert_eq!(h.len(), 1);
        assert_eq!(h[0].kind, PauseKind::Memory);
        assert!(h[0].text().contains("memory (swap 8.4/9.0 GB and growing, 1,200 pageouts/min)"), "{}", h[0].text());
        // Still growing a little (over the release line): still held.
        assert_eq!(hold_step(&t.state(), &r(8700, Some(40), Some(1_200)), &st).len(), 1);
        // High but flat, nothing paging: released.
        assert!(hold_step(&t.state(), &r(8700, Some(5), Some(300)), &st).is_empty());
        // Between the lines: growing 50 MB/min does not hold afresh.
        assert!(hold_step(&t.state(), &r(8700, Some(50), Some(300)), &st).is_empty());
        // Growing fast but swap is mostly empty: the level is not the problem.
        assert!(hold_step(&t.state(), &r(2000, Some(500), Some(300)), &st).is_empty());
        // No earlier sample, no growth, no hold: a gauge not read does not hold.
        assert!(hold_step(&t.state(), &r(8700, None, None), &st).is_empty());
    }

    #[test]
    fn a_high_pageout_rate_holds_alone_and_lets_go_under_its_lower_line() {
        let t = crate::util::isolated("throttle-hold-pageouts");
        let st = Settings::default();
        let r = |po: u32| Reading { pageouts_min: Some(po), ..calm() };
        assert!(hold_step(&t.state(), &r(19_000), &st).is_empty());
        let h = hold_step(&t.state(), &r(25_000), &st);
        assert_eq!(h[0].kind, PauseKind::Memory);
        assert!(h[0].text().contains("swap 4.0/5.0 GB, 25,000 pageouts/min"), "{}", h[0].text());
        assert!(!h[0].text().contains("and growing"));
        assert_eq!(hold_step(&t.state(), &r(8_000), &st).len(), 1, "under the hold line, over the release line");
        assert!(hold_step(&t.state(), &r(4_000), &st).is_empty());
    }

    #[test]
    fn the_free_percentage_is_shown_and_gates_nothing() {
        let t = crate::util::isolated("throttle-hold-free-pct");
        let st = Settings::default();
        // Free percentage at 2% with nothing paging and swap flat: not held.
        assert!(hold_step(&t.state(), &Reading { mem_free: Some(2), ..calm() }, &st).is_empty());
        let j = memory_json_of(&Reading { mem_free: Some(32), swap_growth_mb_min: Some(120), ..calm() }, None);
        assert_eq!(j["free_percent"], 32);
        assert_eq!(j["swap_used_mb"], 4096);
        assert_eq!(j["swap_total_mb"], 5120);
        assert_eq!(j["swap_growth_mb_per_min"], 120);
        assert_eq!(j["pageouts_per_min"], 1_500);
        assert_eq!(j["held"], false);
    }

    #[test]
    fn the_clock_holds_with_its_own_release_line_and_no_gauge_no_hold() {
        let t = crate::util::isolated("throttle-hold-heat");
        let st = Settings::default();
        let r = |therm: u32| Reading { thermal: Some(therm), ..calm() };
        assert!(hold_step(&t.state(), &r(100), &st).is_empty());
        let h = hold_step(&t.state(), &r(31), &st);
        assert_eq!(h[0].kind, PauseKind::Thermal);
        assert_eq!(hold_step(&t.state(), &r(72), &st).len(), 1, "still under 75");
        assert!(hold_step(&t.state(), &r(75), &st).is_empty());
        let blind = Reading { one: 0.0, cores: 8, mem_free: None, swap: None, swap_growth_mb_min: None, pageouts_min: None, thermal: None };
        assert!(hold_step(&t.state(), &blind, &st).is_empty());
    }

    #[test]
    fn rates_are_taken_over_a_window_against_the_last_sample() {
        let t = crate::util::isolated("throttle-mem-rates");
        let state = t.state();
        // The first sample has nothing to compare with.
        assert_eq!(mem_rates(&state, 1000, 4000, 1_000_000), (None, None));
        // A call two seconds later reuses the rates (none) and keeps the baseline.
        assert_eq!(mem_rates(&state, 1002, 4010, 1_000_100), (None, None));
        // 30 s on: +200 MB of swap and 20,000 pages out in half a minute.
        assert_eq!(mem_rates(&state, 1030, 4200, 1_020_000), (Some(400), Some(40_000)));
        // Soon after, the same rates stand.
        assert_eq!(mem_rates(&state, 1035, 4300, 1_020_100), (Some(400), Some(40_000)));
        // Against the 1030 baseline, a minute on: swap falling is a negative growth.
        assert_eq!(mem_rates(&state, 1090, 3900, 1_020_100), (Some(-300), Some(100)));
        // A counter that went back (reboot) gives no rate; so does a stale mark.
        assert_eq!(mem_rates(&state, 1150, 100, 5), (None, None));
        assert_eq!(mem_rates(&state, 1150 + MEM_STALE_SECS + 1, 100, 50), (None, None));
    }

    #[test]
    fn swap_and_pageouts_are_read_out_of_their_tools() {
        assert_eq!(
            parse_swapusage("vm.swapusage: total = 9216.00M  used = 8602.50M  free = 613.50M  (encrypted)"),
            Some((8603, 9216))
        );
        assert_eq!(parse_swapusage("total = 8.00G  used = 6.25G  free = 1.75G"), Some((6400, 8192)));
        assert_eq!(parse_swapusage("nothing"), None);
        let vm = "Mach Virtual Memory Statistics: (page size of 16384 bytes)\nPages free:  2919.\nPageouts:   1000.\n";
        assert_eq!(parse_pageouts(vm), Some(4000), "in 4 KB pages");
        assert_eq!(parse_pageouts("Mach Virtual Memory Statistics: (page size of 4096 bytes)\nPageouts:  12205611.\n"), Some(12205611));
        assert_eq!(parse_pageouts("nothing"), None);
    }

    #[test]
    fn memory_free_is_read_out_of_memory_pressure() {
        assert_eq!(parse_mem_free("Pageouts: 1 \n\nSystem-wide memory free percentage: 57%\n"), Some(57));
        assert_eq!(parse_mem_free("nothing"), None);
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
            thermal: &|| None,
            alive: &alive,
            say,
            poll: Duration::from_millis(20),
            held: false,
            env,
            log: &|_, _| {},
            body: &|_| String::new(),
        }
    }

    /// The niceness the test process itself runs at: `nice -n 12 cargo test`
    /// hands every fake cargo that base, throttled or not.
    fn base_nice() -> i32 {
        crate::host::LOCAL.rows_of(&[std::process::id()], "ni=").unwrap().trim().parse().unwrap()
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

    const PMSET: &str = "Note: No thermal warning level has been recorded\n2026-11-02 12:00:00 +0100 CPU Power notify\n\tCPU_Scheduler_Limit \t= 100\n\tCPU_Available_CPUs \t= 8\n\tCPU_Speed_Limit \t= 31\n";

    #[test]
    fn the_speed_limit_is_read_out_of_pmset() {
        assert_eq!(parse_speed_limit(PMSET), Some(31));
        assert_eq!(parse_speed_limit("Note: No thermal warning level has been recorded\n"), None);
        assert_eq!(parse_speed_limit("\tCPU_Speed_Limit \t= x\n"), None);
    }

    #[test]
    fn a_hot_machine_gets_the_measured_slots_and_jobs_never_above_what_a_hand_set() {
        let st = Settings::default();
        assert!(st.is_hot(Some(31)) && !st.is_hot(Some(100)) && !st.is_hot(Some(THERMAL_BELOW)) && !st.is_hot(None));
        assert_eq!((st.slots_at(8, false), st.jobs_at(8, false)), (2, 3));
        assert_eq!((st.slots_at(8, true), st.jobs_at(8, true)), (HOT_SLOTS, HOT_JOBS), "the measured best, which is more slots than a quarter of the cores");
        let low = Settings { slots: Some(1), jobs: Some(1), ..Settings::default() };
        assert_eq!((low.slots_at(8, true), low.jobs_at(8, true)), (1, 1), "never above what was asked for");
        let own = Settings { hot_slots: Some(3), hot_jobs: Some(5), ..Settings::default() };
        assert_eq!((own.slots_at(8, true), own.jobs_at(8, true)), (3, 5), "and the hand's own number wins");
        assert_eq!(st.hot_agents(8), HOT_SLOTS);
    }

    #[test]
    fn the_thermal_settings_are_kept_in_the_file() {
        let t = crate::util::isolated("throttle-hot-settings");
        let st = Settings { thermal_below: 55, hot_slots: Some(1), hot_jobs: Some(3), ..Settings::default() };
        st.save(&t.state()).unwrap();
        let back = Settings::load(&t.state());
        assert_eq!((back.thermal_below, back.hot_slots, back.hot_jobs), (55, Some(1), Some(3)));
    }

    #[test]
    fn off_still_caps_the_jobs_of_a_thermally_limited_machine_and_does_not_nice_it() {
        let rig = rig("throttle-run-off-hot");
        let st = Settings { mode: Mode::Off, slots: Some(4), jobs: Some(4), hot_jobs: Some(1), ..Settings::default() };
        st.save(&rig.state).unwrap();
        let (idle, say, env, hot) = (|| Some(0), |_: &str| {}, |_: &str| None, || Some(31));
        let mut c = ctx(&rig, &idle, &say, &env);
        c.thermal = &hot;
        run_in(&c, "cargo", &s(&["test"]));
        let line = &seen(&rig)[0];
        assert!(line.contains("jobs=1") && at_base_nice(line), "{line}");
    }

    #[test]
    fn off_still_waits_for_the_hot_machines_slot() {
        let rig = rig("throttle-run-off-hot-slot");
        let st = Settings { mode: Mode::Off, slots: Some(4), jobs: Some(4), hot_slots: Some(1), ..Settings::default() };
        st.save(&rig.state).unwrap();
        fs::create_dir_all(builds_dir(&rig.state)).unwrap();
        fs::write(slot_path(&rig.state, 0), rec_for(std::process::id()).to_string()).unwrap();
        let cooled = std::cell::Cell::new(false);
        let said = RefCell::new(Vec::new());
        let say = |m: &str| {
            said.borrow_mut().push(m.to_string());
            cooled.set(true);
        };
        let (idle, env) = (|| Some(0), |_: &str| None);
        let thermal = || Some(if cooled.get() { 100 } else { 31 });
        let mut c = ctx(&rig, &idle, &say, &env);
        c.thermal = &thermal;
        assert_eq!(run_in(&c, "cargo", &s(&["build"])), 0);
        assert!(said.borrow()[0].contains("waiting for a build slot"), "{:?}", said.borrow());
        assert_eq!(seen(&rig).len(), 1, "and the wait ends when the machine cools");
    }

    #[test]
    fn a_cool_machine_in_off_takes_no_slot() {
        let rig = rig("throttle-run-off-cool");
        let st = Settings { mode: Mode::Off, slots: Some(1), hot_slots: Some(1), ..Settings::default() };
        st.save(&rig.state).unwrap();
        fs::create_dir_all(builds_dir(&rig.state)).unwrap();
        fs::write(slot_path(&rig.state, 0), rec_for(std::process::id()).to_string()).unwrap();
        let (idle, say, env, cool) = (|| Some(0), |_: &str| panic!("it waited"), |_: &str| None, || Some(100));
        let mut c = ctx(&rig, &idle, &say, &env);
        c.thermal = &cool;
        assert_eq!(run_in(&c, "cargo", &s(&["build"])), 0);
    }

    #[test]
    fn a_hot_view_says_so_and_suggests_fewer_agents() {
        let t = crate::util::isolated("throttle-hot-view");
        let mut v = View::read(&t.state());
        // The real load is the fleet's, and a busy one is a warning of its own.
        v.load = None;
        v.thermal = Some(31);
        assert!(v.hot());
        assert!(v.status_line().contains("cpu clock 31% — thermally limited"), "{}", v.status_line());
        assert!(v.render().contains("thermally limited, so in every mode"), "{}", v.render());
        let w = v.warning(None).unwrap();
        assert!(w.contains("31%") && w.contains("wsp machine set seat agents="), "{w}");
        assert!(!v.warning(Some(1)).unwrap().contains("wsp machine set"), "a cap already low enough is left alone");
        v.thermal = Some(90);
        assert!(!v.hot() && v.warning(None).is_none());
        assert!(v.status_line().contains("cpu clock 90%") && !v.status_line().contains("limited"));
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
        assert!(is_full_suite("cargo", &a(&["nextest", "run"])));
        assert!(is_full_suite("cargo", &a(&["nextest", "run", "--release"])));
        assert!(!is_full_suite("cargo", &a(&["nextest", "run", "cmd_spawn::"])));
        assert!(!is_full_suite("cargo", &a(&["nextest", "run", "-p", "x"])));
        assert!(!is_full_suite("cargo", &a(&["nextest", "list"])));
        assert!(!is_full_suite("npm", &a(&["test"])));
    }

    #[test]
    fn a_full_suite_in_a_rows_seat_is_noted_and_counted_and_a_filtered_one_is_not() {
        let rig = rig("throttle-full-suite");
        mode(&rig, Mode::Off, 1);
        let idle = || None;
        let said = std::cell::RefCell::new(Vec::<String>::new());
        let say = |m: &str| said.borrow_mut().push(m.to_string());
        let env = |k: &str| (k == crate::base::seat::SEAT_ENV).then(|| "w1:p1".to_string());
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

    /// `wsp-538`: a member confirms with a reason before the whole suite; the
    /// reason is the row's log; a verifier and a check are not asked.
    #[test]
    fn a_members_full_nextest_is_refused_and_allowed_with_a_reason_that_lands_in_the_log() {
        use std::os::unix::fs::PermissionsExt;
        let rig = rig("throttle-member-suite");
        mode(&rig, Mode::Off, 1);
        fs::write(rig.bin.join("cargo"), "#!/bin/sh\nexit 0\n").unwrap();
        fs::set_permissions(rig.bin.join("cargo"), fs::Permissions::from_mode(0o755)).unwrap();
        let idle = || None;
        let said = std::cell::RefCell::new(Vec::<String>::new());
        let say = |m: &str| said.borrow_mut().push(m.to_string());
        let logged = std::cell::RefCell::new(Vec::<(String, String)>::new());
        let log = |t: &str, l: &str| logged.borrow_mut().push((t.to_string(), l.to_string()));
        let body = |_: &str| "## Checks\n\ntests: cargo test cmd_spawn::\n\n## Log\n\n- 1 barrier: FAIL [   0.2s] wsp cmd_spawn::a_seat_starts\n".to_string();
        let why = std::cell::RefCell::new(None::<String>);
        let kind = std::cell::RefCell::new("member".to_string());
        let env = |k: &str| match k {
            SEAT_KIND => Some(kind.borrow().clone()),
            FULL_SUITE_WHY => why.borrow().clone(),
            _ => None,
        };
        let mut c = ctx(&rig, &idle, &say, &env);
        c.log = &log;
        c.body = &body;
        let full = s(&["nextest", "run", "--workspace"]);

        assert_eq!(run_in(&c, "cargo", &full), 2, "refused");
        let text = said.borrow().join("\n");
        assert!(text.contains("barrier runs the full suite once per group") && text.contains("wsp check <row>") && text.contains("WSP_FULL_SUITE"), "{text}");
        assert!(text.contains("tests: cargo test cmd_spawn::"), "the row's own checks: {text}");
        assert!(text.contains("cargo nextest run -E 'test(=cmd_spawn::a_seat_starts)'"), "the failing test, alone: {text}");
        assert_eq!(full_suite_runs(&rig.state, "wsp-9"), 0, "a refused run is not a run");
        assert!(logged.borrow().is_empty());

        assert_eq!(run_in(&c, "cargo", &s(&["nextest", "run", "cmd_spawn::"])), 0, "a filtered run is the member's own");
        assert_eq!(run_in(&c, "cargo", &s(&["test", "--workspace"])), 2, "plain cargo test too");

        *why.borrow_mut() = Some("the fixture every test reads".into());
        assert_eq!(run_in(&c, "cargo", &full), 0, "allowed with a reason");
        assert_eq!(*logged.borrow(), vec![("wsp-9".to_string(), format!("{FULL_SUITE_LOGGED} the fixture every test reads"))]);
        assert_eq!(full_suite_runs(&rig.state, "wsp-9"), 1, "and still counted");

        *kind.borrow_mut() = "verifier".into();
        *why.borrow_mut() = None;
        assert_eq!(run_in(&c, "cargo", &full), 0, "a verifier is not gated");
        *kind.borrow_mut() = "check".into();
        assert_eq!(run_in(&c, "cargo", &full), 0, "a check is not gated");
        let mut person = ctx(&rig, &idle, &say, &env);
        person.task = String::new();
        *kind.borrow_mut() = "member".into();
        assert_eq!(run_in(&person, "cargo", &full), 0, "a person at a shell has no row");
    }

    #[test]
    fn a_done_when_requiring_the_full_workspace_run_is_found_and_a_scoped_one_is_not() {
        for yes in [
            "`cargo nextest run --workspace` runs to its end",
            "Done when: `cargo test` is green.",
            "verify  cargo test",
            "the full `npm run check` passes",
        ] {
            assert!(asks_full_suite(yes).is_some(), "{yes}");
        }
        for no in [
            "`cargo nextest run -E 'test(=a::b)'` passes",
            "verify  cargo test cmd_worklist::",
            "cargo test is slow, so run the one test",
            "`cargo nextest run -p wsp a::`",
            "Save prints ok.",
        ] {
            assert!(asks_full_suite(no).is_none(), "{no}: {:?}", asks_full_suite(no));
        }
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
            thermal: None,
            builds: builds(&t.state(), &|p| p == me),
            checks: Checks::default(),
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
                thermal: None,
                builds: vec![],
                checks: Checks::default(),
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
            thermal: None,
            builds: vec![Build { holder: rec_for(1), slot: Some(0) }],
            checks: Checks::default(),
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
                thermal: None,
                builds: vec![],
                checks: Checks::default(),
            shims: PathBuf::from("/s"),
            };
            assert!(v.load_warning() && v.render().contains("hold off heavy runs"), "{m:?}");
        }
    }

    fn gate<'a>(
        state: &'a Path,
        thermal: &'a dyn Fn() -> Option<u32>,
        alive: &'a dyn Fn(u32) -> bool,
        say: &'a dyn Fn(&str),
    ) -> CheckGate<'a> {
        CheckGate {
            state,
            cores: 8,
            task: "wsp-9".into(),
            cmd: "sleep 1".into(),
            dir: state.to_path_buf(),
            thermal,
            alive,
            say,
            poll: Duration::from_millis(10),
        }
    }

    #[test]
    fn a_fifth_check_waits_for_one_of_four_slots_on_a_limited_clock_and_starts_when_one_ends() {
        let t = crate::util::isolated("throttle-check-slots");
        let state = t.state();
        Settings { checks: Some(4), ..Settings::default() }.save(&state).unwrap();
        let (thermal, say) = (|| Some(40u32), |_: &str| {});
        let here = |_: u32| true;
        let mut holds: Vec<Hold> = (0..4).map(|_| take_check_slot(&gate(&state, &thermal, &here, &say))).collect();
        let started = std::sync::Arc::new(AtomicBool::new(false));
        let waiter = {
            let (state, started) = (state.clone(), started.clone());
            std::thread::spawn(move || {
                let (thermal, say, here) = (|| Some(40u32), |_: &str| {}, |_: u32| true);
                let h = take_check_slot(&gate(&state, &thermal, &here, &say));
                started.store(true, Ordering::SeqCst);
                drop(h);
            })
        };
        std::thread::sleep(Duration::from_millis(300));
        assert!(!started.load(Ordering::SeqCst), "the fifth check started with four running");
        let seen = checks(&state, &here);
        assert_eq!(seen.running.len(), 4, "{seen:?}");
        assert_eq!(seen.waiting.len(), 1, "the fifth is named as waiting: {seen:?}");
        holds.pop();
        waiter.join().unwrap();
        assert!(started.load(Ordering::SeqCst), "it started when a slot was freed");
        assert!(checks(&state, &here).waiting.is_empty(), "and is no longer waiting");
    }

    #[test]
    fn the_check_slots_shrink_with_the_clock_and_off_lifts_them_on_a_cool_machine() {
        let s = Settings::default();
        assert_eq!(s.check_slots_at(8, false), 4);
        assert_eq!(s.check_slots_at(16, false), 8);
        assert_eq!(s.check_slots_at(16, true), HOT_SLOTS, "a limited clock holds the count down");
        assert_eq!(s.check_slots_at(2, true), 2.min(HOT_SLOTS));
        let t = crate::util::isolated("throttle-check-off");
        let state = t.state();
        Settings { mode: Mode::Off, checks: Some(1), ..Settings::default() }.save(&state).unwrap();
        let (cool, hot, say, here) = (|| Some(100u32), || Some(30u32), |_: &str| {}, |_: u32| true);
        let _a = take_check_slot(&gate(&state, &cool, &here, &say));
        let _b = take_check_slot(&gate(&state, &cool, &here, &say));
        assert!(checks(&state, &here).running.is_empty(), "off on a cool machine takes no slot");
        let _c = take_check_slot(&gate(&state, &hot, &here, &say));
        assert_eq!(checks(&state, &here).running.len(), 1, "a limited clock holds it in every mode");
    }

    #[test]
    fn the_view_names_the_check_slots_and_what_waits_on_them() {
        let v = View {
            settings: Settings { checks: Some(4), ..Settings::default() },
            idle: Some(1),
            in_force: true,
            cores: 8,
            load: None,
            thermal: Some(40),
            builds: vec![],
            checks: Checks { running: vec![(0, rec_for(1)), (1, rec_for(1))], waiting: vec![rec_for(1)] },
            shims: PathBuf::from("/s"),
        };
        let line = v.status_line();
        assert!(line.contains("check slots 2/4, waiting: wsp-9 `cargo test`"), "{line}");
        let out = v.render();
        assert!(out.contains("checks: 4 at once machine-wide (the clock is limited)"), "{out}");
        assert!(out.contains("check slot 2/4") && out.contains("waiting for a check slot"), "{out}");
    }
}
