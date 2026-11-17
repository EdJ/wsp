//! Resident memory held by idle agent panes, and a warning past a threshold:
//! `wsp-338`, piece 3 of `wsp-332`.
//!
//! On 2026-11-01 seventeen idle `claude` panes held 150 to 300 MB each and the
//! machine paged until it was unusable; nothing on `wsp wip` said so. [`read`]
//! sums the resident memory of every process in each idle seat, and
//! [`IdleMemory`] is what `wsp wip` and `wsp watch --status` draw from it.
//!
//! A seat's processes are found two ways, because the backends differ: a
//! backend that names a root pid (compound) is walked down from that pid, and a
//! backend pane is every process whose environment carries its `WSP_BACKEND_PANE_ID` —
//! backend names no pid, but every descendant of a pane inherits the variable.
//!
//! The threshold is `idle-memory.json` under the state directory
//! (`{"warn_mb": 4096}`), or `WSP_IDLE_MEMORY_MB`, which wins.

use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;

use serde_json::{json, Value};

use crate::place::{Seated, State};

/// Idle panes holding more than this, in MB, are warned about.
pub(crate) const DEFAULT_WARN_MB: u64 = 2048;

/// How many of the largest panes the warning names.
const NAMED: usize = 3;

/// One idle pane and what its processes hold.
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct Pane {
    pub seat: String,
    /// What a person knows it by: its task when it holds one, else its label.
    pub name: String,
    pub kb: u64,
}

/// Every idle pane that could be measured, largest first, and the threshold.
#[derive(Debug, Clone, Default)]
pub(crate) struct IdleMemory {
    pub panes: Vec<Pane>,
    /// Idle seats whose processes were not found here — a far machine's, or a
    /// `ps` that did not run. Counted so "0 MB" is never read as "nothing held".
    pub unread: usize,
    pub warn_kb: u64,
}

impl IdleMemory {
    pub(crate) fn total_kb(&self) -> u64 {
        self.panes.iter().map(|p| p.kb).sum()
    }

    /// Past the threshold, not at it.
    pub(crate) fn over(&self) -> bool {
        self.total_kb() > self.warn_kb
    }

    /// The line `wsp wip` and `wsp watch --status` carry every time.
    pub(crate) fn line(&self) -> String {
        let unread = match self.unread {
            0 => String::new(),
            n => format!(" ({n} not readable here)"),
        };
        match self.panes.len() {
            0 if self.unread == 0 => "idle panes: none".to_string(),
            0 => format!("idle panes: memory not readable{unread}"),
            n => format!("idle panes: {n} holding {}{unread}", size(self.total_kb())),
        }
    }

    /// The warning, when the total is past the threshold: the total and the
    /// largest panes by name.
    pub(crate) fn warning(&self) -> Option<String> {
        self.over().then(|| {
            let largest: Vec<String> = self.panes.iter().take(NAMED).map(|p| format!("{} ({})", p.name, size(p.kb))).collect();
            format!(
                "idle panes hold {}, over the {} warning — largest: {}",
                size(self.total_kb()),
                size(self.warn_kb),
                largest.join(", ")
            )
        })
    }

    pub(crate) fn json(&self) -> Value {
        json!({
            "total_kb": self.total_kb(),
            "warn_kb": self.warn_kb,
            "over": self.over(),
            "unread": self.unread,
            "panes": self.panes.iter().map(|p| json!({ "seat": p.seat, "name": p.name, "kb": p.kb })).collect::<Vec<_>>(),
        })
    }
}

fn size(kb: u64) -> String {
    match kb {
        k if k >= 1024 * 1024 => format!("{:.1} GB", k as f64 / (1024.0 * 1024.0)),
        k => format!("{} MB", k / 1024),
    }
}

/// The threshold in kilobytes: the environment, then the state file, then
/// [`DEFAULT_WARN_MB`].
pub(crate) fn warn_kb(state: &Path) -> u64 {
    let env = std::env::var("WSP_IDLE_MEMORY_MB").ok();
    let file = std::fs::read_to_string(state.join("idle-memory.json")).ok();
    warn_mb_from(env.as_deref(), file.as_deref()) * 1024
}

fn warn_mb_from(env: Option<&str>, file: Option<&str>) -> u64 {
    env.and_then(|e| e.trim().parse().ok())
        .or_else(|| file.and_then(|f| serde_json::from_str::<Value>(f).ok())?.get("warn_mb")?.as_u64())
        .unwrap_or(DEFAULT_WARN_MB)
}

/// The idle seats among `agents`, each with the name a person knows it by:
/// its task when it holds one, else its label.
fn idle(agents: &[Seated], bindings: &BTreeMap<String, Value>) -> Vec<(String, String)> {
    agents
        .iter()
        .filter(|a| a.state == State::Idle)
        .map(|a| {
            let seat = a.seat.as_str().to_string();
            let task = bindings.get(&seat).and_then(|b| b.get("task_id")).and_then(Value::as_str).filter(|t| !t.is_empty());
            let name = task.map(str::to_string).or_else(|| Some(a.label.clone()).filter(|l| !l.is_empty())).unwrap_or_else(|| seat.clone());
            (seat, name)
        })
        .collect()
}

/// The idle panes among `agents`, measured by their managers: `rss` is each
/// seat's resident bytes as its `seats` reply said (`docs/agent-manager.md`
/// 4.3), so nothing here asks the process table. A seat no manager measured
/// is `unread`, as a far one is.
pub(crate) fn of_rss(state: &Path, agents: &[Seated], bindings: &BTreeMap<String, Value>, rss: &BTreeMap<String, u64>) -> IdleMemory {
    let warn_kb = warn_kb(state);
    let mut panes = Vec::new();
    let mut unread = 0;
    for (seat, name) in idle(agents, bindings) {
        match rss.get(&seat).map(|b| b / 1024).unwrap_or(0) {
            0 => unread += 1,
            kb => panes.push(Pane { seat, name, kb }),
        }
    }
    panes.sort_by(|a, b| b.kb.cmp(&a.kb).then_with(|| a.seat.cmp(&b.seat)));
    IdleMemory { panes, unread, warn_kb }
}

/// The idle panes among `agents`, measured. `bindings` names a seat's task.
///
/// Asks the process table once, through `ps` (the caller's Host's
/// `seat_memory`, passed in because the Host sits a layer above this module),
/// and not at all with no idle seat. Under test there is no table to ask:
/// tests hand one to [`of`].
pub(crate) fn read(
    state: &Path,
    agents: &[Seated],
    bindings: &BTreeMap<String, Value>,
    ps: impl FnOnce() -> Option<String>,
) -> IdleMemory {
    let seats = idle(agents, bindings);
    let warn_kb = warn_kb(state);
    if seats.is_empty() || cfg!(test) {
        return IdleMemory { warn_kb, unread: seats.len(), ..IdleMemory::default() };
    }
    let backends = crate::cmd_spawn::local_backends();
    let roots: Vec<(String, u32)> = seats
        .iter()
        .filter_map(|(s, _)| {
            let seat = crate::place::Seat::new(s);
            backends.iter().find_map(|b| b.root(&seat)).map(|pid| (s.clone(), pid))
        })
        .collect();
    // `ps -E` is BSD-only: elsewhere the same rows come from `/proc`.
    #[cfg(target_os = "macos")]
    let table = ps();
    #[cfg(not(target_os = "macos"))]
    let table = {
        let _ = ps;
        table()
    };
    match table {
        Some(t) => of(&t, &seats, &roots, warn_kb),
        None => IdleMemory { warn_kb, unread: seats.len(), ..IdleMemory::default() },
    }
}

/// The process table in the shape [`of`] reads, where `ps -E` is not (it is
/// BSD-only): the same rows from `/proc`. Only the one
/// variable [`of`] looks for is copied out of `environ`, and a process whose
/// `environ` is not ours to read is a row without a pane.
#[cfg(not(target_os = "macos"))]
fn table() -> Option<String> {
    let mut out = String::new();
    for e in std::fs::read_dir("/proc").ok()?.flatten() {
        let Some(pid) = e.file_name().to_str().and_then(|n| n.parse::<u32>().ok()) else { continue };
        let dir = e.path();
        let Ok(status) = std::fs::read_to_string(dir.join("status")) else { continue };
        let Some((ppid, kb)) = proc_status(&status) else { continue };
        let pane = std::fs::read(dir.join("environ")).ok().and_then(|b| pane_from_environ(&b));
        out.push_str(&format!("{pid} {ppid} {kb} proc{}\n", pane.map(|p| format!(" WSP_BACKEND_PANE_ID={p}")).unwrap_or_default()));
    }
    Some(out)
}

/// `PPid` and `VmRSS` (kB) from a `/proc/<pid>/status` body. A kernel thread
/// has no `VmRSS` and counts as 0.
#[cfg_attr(target_os = "macos", allow(dead_code))]
fn proc_status(status: &str) -> Option<(u32, u64)> {
    let field = |k: &str| status.lines().find_map(|l| l.strip_prefix(k)).map(str::trim);
    let ppid = field("PPid:")?.parse().ok()?;
    let kb = field("VmRSS:").and_then(|v| v.split_whitespace().next()).and_then(|n| n.parse().ok()).unwrap_or(0);
    Some((ppid, kb))
}

/// `WSP_BACKEND_PANE_ID` out of a NUL-separated `/proc/<pid>/environ`.
#[cfg_attr(target_os = "macos", allow(dead_code))]
fn pane_from_environ(environ: &[u8]) -> Option<String> {
    environ
        .split(|b| *b == 0)
        .find_map(|kv| kv.strip_prefix(b"WSP_BACKEND_PANE_ID="))
        .map(|v| String::from_utf8_lossy(v).into_owned())
        .filter(|v| !v.is_empty() && !v.contains(char::is_whitespace))
}

/// The reading over a `ps -E -o pid=,ppid=,rss=,command=` table. `seats` is
/// seat and name; `roots` is the seats whose backend names a pid.
fn of(ps: &str, seats: &[(String, String)], roots: &[(String, u32)], warn_kb: u64) -> IdleMemory {
    struct Row<'a> {
        pid: u32,
        ppid: u32,
        kb: u64,
        pane: Option<&'a str>,
    }
    let rows: Vec<Row> = ps
        .lines()
        .filter_map(|l| {
            let mut it = l.split_whitespace();
            let pid = it.next()?.parse().ok()?;
            let ppid = it.next()?.parse().ok()?;
            let kb = it.next()?.parse().ok()?;
            let pane = it.find_map(|w| w.strip_prefix("WSP_BACKEND_PANE_ID=")).filter(|v| !v.is_empty());
            Some(Row { pid, ppid, kb, pane })
        })
        .collect();
    let mut panes = Vec::new();
    let mut unread = 0;
    for (seat, name) in seats {
        let mut mine: BTreeSet<u32> = rows.iter().filter(|r| r.pane == Some(seat.as_str())).map(|r| r.pid).collect();
        if let Some((_, root)) = roots.iter().find(|(s, _)| s == seat) {
            let mut tree = vec![*root];
            let mut i = 0;
            while i < tree.len() {
                let parent = tree[i];
                tree.extend(rows.iter().filter(|r| r.ppid == parent && !tree.contains(&r.pid)).map(|r| r.pid).collect::<Vec<_>>());
                i += 1;
            }
            mine.extend(tree);
        }
        let kb: u64 = rows.iter().filter(|r| mine.contains(&r.pid)).map(|r| r.kb).sum();
        match kb {
            0 => unread += 1,
            kb => panes.push(Pane { seat: seat.clone(), name: name.clone(), kb }),
        }
    }
    panes.sort_by(|a, b| b.kb.cmp(&a.kb).then_with(|| a.seat.cmp(&b.seat)));
    IdleMemory { panes, unread, warn_kb }
}

// Each seat's resident memory over its seated time (`wsp-066`).
//
// The reading above is a moment: what idle panes hold now. What a governor or
// a check seat costs while it waits is a figure over its whole seat, and the
// seat is gone by the time a retro asks — `ps` has nothing to say of it then.
// So the daemon samples every seated agent's resident memory on its own
// interval and folds it into `seat-memory.json`: per seat, stretches of
// samples, each with the KB·seconds under it (trapezoids between samples),
// its peak and its last reading. A gap longer than [`BRIDGE`] (the daemon
// down, the seat gone and its name taken again) starts a new stretch, so it is
// never bridged with a figure nobody read.

/// The file under the state directory.
pub(crate) const SEAT_MEMORY_FILE: &str = "seat-memory.json";

/// How often the daemon samples, in seconds.
pub(crate) const SAMPLE_EVERY: i64 = 60;

/// The longest gap between two samples that is read as one stretch.
const BRIDGE: i64 = 3 * SAMPLE_EVERY;

/// Stretches whose last sample is older than this are dropped from the file.
const KEEP: i64 = 30 * 86_400;

/// Samples every seated agent among `agents` through one `ps` and banks them.
/// Under test there is no table to ask: tests fold readings with [`fold`].
pub(crate) fn sample(state: &Path, agents: &[Seated], ps: impl FnOnce() -> Option<String>, now: i64) {
    let seats: Vec<(String, String)> =
        agents.iter().filter(|a| a.state != State::Empty).map(|a| (a.seat.as_str().to_string(), a.seat.as_str().to_string())).collect();
    if seats.is_empty() || cfg!(test) {
        return;
    }
    let backends = crate::cmd_spawn::local_backends();
    let roots: Vec<(String, u32)> = seats
        .iter()
        .filter_map(|(s, _)| {
            let seat = crate::place::Seat::new(s);
            backends.iter().find_map(|b| b.root(&seat)).map(|pid| (s.clone(), pid))
        })
        .collect();
    let Some(ps) = ps() else { return };
    let read = of(&ps, &seats, &roots, 0);
    bank(state, &read.panes.iter().map(|p| (p.seat.clone(), p.kb)).collect::<Vec<_>>(), now);
}

/// Banks the managers' readings: `rss` is each seat's resident bytes as its
/// `seats` reply said (`docs/agent-manager.md` 4.3).
pub(crate) fn sample_rss(state: &Path, rss: &BTreeMap<String, u64>, now: i64) {
    let read: Vec<(String, u64)> = rss.iter().map(|(s, b)| (s.clone(), b / 1024)).filter(|(_, kb)| *kb > 0).collect();
    if !read.is_empty() && !cfg!(test) {
        bank(state, &read, now);
    }
}

fn bank(state: &Path, read: &[(String, u64)], now: i64) {
    let path = state.join(SEAT_MEMORY_FILE);
    let prev = std::fs::read_to_string(&path).ok().and_then(|t| serde_json::from_str(&t).ok()).unwrap_or(Value::Null);
    let _ = crate::store::write_atomic_unsynced(&path, &fold(prev, read, now).to_string());
}

/// The file with `read` (seat, KB) sampled at `now` folded in.
pub(crate) fn fold(prev: Value, read: &[(String, u64)], now: i64) -> Value {
    let mut all = match prev {
        Value::Object(m) => m,
        _ => serde_json::Map::new(),
    };
    for (seat, kb) in read.iter().filter(|(_, kb)| *kb > 0) {
        let stretches = all.entry(seat.clone()).or_insert_with(|| json!([]));
        if !stretches.is_array() {
            *stretches = json!([]);
        }
        let list = stretches.as_array_mut().expect("an array");
        let n = |v: &Value, k: &str| v.get(k).and_then(Value::as_i64).unwrap_or(0);
        let last_to = list.last().map(|l| n(l, "to"));
        // A sample at or before the last one (a clock stepped back, two daemons
        // racing) adds nothing.
        if last_to.is_some_and(|t| t >= now) {
            continue;
        }
        match list.last_mut().filter(|_| last_to.is_some_and(|t| now - t <= BRIDGE)) {
            Some(l) => {
                let dt = now - n(l, "to");
                let kb_secs = n(l, "kb_secs") + (n(l, "kb") + *kb as i64) * dt / 2;
                l["kb_secs"] = json!(kb_secs);
                l["to"] = json!(now);
                l["kb"] = json!(kb);
                l["peak_kb"] = json!(n(l, "peak_kb").max(*kb as i64));
                l["samples"] = json!(n(l, "samples") + 1);
            }
            None => list.push(json!({ "from": now, "to": now, "kb": kb, "kb_secs": 0, "peak_kb": kb, "samples": 1 })),
        }
    }
    for stretches in all.values_mut() {
        if let Some(list) = stretches.as_array_mut() {
            list.retain(|l| now - l.get("to").and_then(Value::as_i64).unwrap_or(0) <= KEEP);
        }
    }
    all.retain(|_, v| v.as_array().is_some_and(|l| !l.is_empty()));
    Value::Object(all)
}

/// A seat's resident memory over `[from, to]` (`to == 0`: to now): the
/// stretches sampled that overlap it, whole.
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct SeatMemory {
    pub mb_minutes: f64,
    pub peak_mb: u64,
    /// The seconds the samples span, and how many there were.
    pub sampled_seconds: i64,
    pub samples: u64,
}

impl SeatMemory {
    pub(crate) fn json(&self) -> Value {
        let mean = if self.sampled_seconds > 0 { self.mb_minutes * 60.0 / self.sampled_seconds as f64 } else { self.peak_mb as f64 };
        json!({
            "mb_minutes": (self.mb_minutes * 10.0).round() / 10.0,
            "mean_mb": mean.round() as u64,
            "peak_mb": self.peak_mb,
            "sampled_seconds": self.sampled_seconds,
            "samples": self.samples,
        })
    }
}

/// [`SeatMemory`] read from the state directory's file. `None` where the seat
/// was never sampled inside the span: not measured, which is not zero.
pub(crate) fn seat_memory(state: &Path, seat: &str, from: i64, to: i64) -> Option<SeatMemory> {
    let v: Value = serde_json::from_str(&std::fs::read_to_string(state.join(SEAT_MEMORY_FILE)).ok()?).ok()?;
    seat_memory_in(&v, seat, from, to)
}

pub(crate) fn seat_memory_in(v: &Value, seat: &str, from: i64, to: i64) -> Option<SeatMemory> {
    let to = if to == 0 { i64::MAX } else { to };
    let n = |l: &Value, k: &str| l.get(k).and_then(Value::as_i64).unwrap_or(0);
    let hit: Vec<&Value> = v.get(seat)?.as_array()?.iter().filter(|l| n(l, "to") >= from && n(l, "from") <= to).collect();
    if hit.is_empty() {
        return None;
    }
    Some(SeatMemory {
        mb_minutes: hit.iter().map(|l| n(l, "kb_secs")).sum::<i64>() as f64 / 1024.0 / 60.0,
        peak_mb: hit.iter().map(|l| n(l, "peak_kb")).max().unwrap_or(0) as u64 / 1024,
        sampled_seconds: hit.iter().map(|l| n(l, "to") - n(l, "from")).sum(),
        samples: hit.iter().map(|l| n(l, "samples")).sum::<i64>() as u64,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_seats_samples_fold_into_megabyte_minutes_and_a_gap_is_not_bridged() {
        let s = |kb: u64| vec![("cpd-1".to_string(), kb)];
        let mut v = fold(Value::Null, &s(100 * MB), 1000);
        v = fold(v, &s(300 * MB), 1060);
        v = fold(v, &s(300 * MB), 1120);
        // (100 + 300) / 2 MB for a minute, then 300 MB for a minute.
        let m = seat_memory_in(&v, "cpd-1", 0, 0).unwrap();
        assert_eq!((m.mb_minutes, m.peak_mb, m.sampled_seconds, m.samples), (500.0, 300, 120, 3));
        // The daemon down an hour: a new stretch, with nothing under the gap.
        v = fold(v, &s(200 * MB), 4720);
        assert_eq!(v["cpd-1"].as_array().unwrap().len(), 2, "{v}");
        assert_eq!(seat_memory_in(&v, "cpd-1", 0, 0).unwrap().mb_minutes, 500.0);
        // A span is read off the stretches that overlap it; another seat's
        // never sampled one is not measured.
        assert_eq!(seat_memory_in(&v, "cpd-1", 4000, 0).unwrap().samples, 1);
        assert_eq!(seat_memory_in(&v, "cpd-1", 2000, 3000), None);
        assert_eq!(seat_memory_in(&v, "cpd-2", 0, 0), None);
        assert_eq!(m.json()["mean_mb"], 250);
    }

    #[test]
    fn a_stretch_long_past_is_dropped_and_a_zero_reading_is_not_a_sample() {
        let v = fold(Value::Null, &[("old".to_string(), 50 * MB), ("none".to_string(), 0)], 1000);
        assert!(v.get("none").is_none(), "{v}");
        let v = fold(v, &[("new".to_string(), 50 * MB)], 1000 + KEEP + 1);
        assert!(v.get("old").is_none() && v.get("new").is_some(), "{v}");
    }

    fn seats(names: &[&str]) -> Vec<(String, String)> {
        names.iter().map(|n| (format!("w{n}:p1"), format!("wsp-{n}"))).collect()
    }

    const MB: u64 = 1024;

    #[test]
    fn a_backend_pane_is_every_process_carrying_its_pane_id() {
        let ps = "\
  10     1  1000 /bin/zsh -l WSP_BACKEND_PANE_ID=w1:p1 HOME=/x
  11    10 250000 claude WSP_BACKEND_PANE_ID=w1:p1 HOME=/x
  12    11  50000 node mcp WSP_BACKEND_PANE_ID=w1:p1 HOME=/x
  20     1  1000 /bin/zsh -l WSP_BACKEND_PANE_ID=w2:p1 HOME=/x
  21    20 100000 claude WSP_BACKEND_PANE_ID=w2:p1 HOME=/x
  30     1 900000 something-else HOME=/x
";
        let m = of(ps, &seats(&["1", "2"]), &[], 0);
        assert_eq!(m.panes.len(), 2);
        assert_eq!(m.panes[0], Pane { seat: "w1:p1".into(), name: "wsp-1".into(), kb: 301_000 });
        assert_eq!(m.panes[1].kb, 101_000);
        assert_eq!(m.total_kb(), 402_000);
    }

    #[test]
    fn a_pane_is_not_a_pane_whose_id_it_merely_starts_with() {
        let ps = "  10     1  5000 claude WSP_BACKEND_PANE_ID=w1:p12 HOME=/x\n";
        let m = of(ps, &seats(&["1"]), &[], 0);
        assert!(m.panes.is_empty());
        assert_eq!(m.unread, 1);
    }

    #[test]
    fn a_root_pid_is_walked_down_and_counted_once_beside_the_pane_id() {
        let ps = "\
  10     1  1000 compound-sup
  11    10 200000 claude WSP_BACKEND_PANE_ID=w1:p1
  12    11  30000 node
  13     1 777000 unrelated
";
        let m = of(ps, &seats(&["1"]), &[("w1:p1".into(), 10)], 0);
        assert_eq!(m.total_kb(), 231_000, "the claude is in the tree and tagged, and counted once");
    }

    #[test]
    fn proc_files_read_as_rows() {
        assert_eq!(proc_status("Name:\tx\nPPid:\t7\nVmRSS:\t  1234 kB\n"), Some((7, 1234)));
        assert_eq!(proc_status("Name:\tkthreadd\nPPid:\t0\n"), Some((0, 0)));
        assert_eq!(pane_from_environ(b"A=1\0WSP_BACKEND_PANE_ID=w1:p1\0B=2\0"), Some("w1:p1".into()));
        assert_eq!(pane_from_environ(b"XWSP_BACKEND_PANE_ID=w1:p1\0"), None);
        assert_eq!(pane_from_environ(b"WSP_BACKEND_PANE_ID=\0"), None);
    }

    #[test]
    fn the_largest_panes_come_first() {
        let ps = "  1 0 100 a WSP_BACKEND_PANE_ID=w1:p1\n  2 0 300 b WSP_BACKEND_PANE_ID=w2:p1\n  3 0 200 c WSP_BACKEND_PANE_ID=w3:p1\n";
        let m = of(ps, &seats(&["1", "2", "3"]), &[], 0);
        assert_eq!(m.panes.iter().map(|p| p.kb).collect::<Vec<_>>(), vec![300, 200, 100]);
    }

    #[test]
    fn a_seat_with_no_process_here_is_unread_and_not_zero_megabytes() {
        let m = of("  1 0 100 a\n", &seats(&["1"]), &[], 0);
        assert_eq!((m.panes.len(), m.unread), (0, 1));
        assert_eq!(m.line(), "idle panes: memory not readable (1 not readable here)");
    }

    fn held(mb: &[u64], warn_mb: u64) -> IdleMemory {
        IdleMemory {
            panes: mb.iter().enumerate().map(|(i, m)| Pane { seat: format!("w{i}:p1"), name: format!("wsp-{i}"), kb: m * MB }).collect(),
            unread: 0,
            warn_kb: warn_mb * MB,
        }
    }

    #[test]
    fn the_total_shows_with_no_warning_under_the_threshold() {
        let m = held(&[300, 200, 100], 1000);
        assert_eq!(m.line(), "idle panes: 3 holding 600 MB");
        assert!(m.warning().is_none());
    }

    #[test]
    fn exactly_the_threshold_is_not_past_it() {
        assert!(held(&[500, 500], 1000).warning().is_none());
        assert!(held(&[500, 501], 1000).warning().is_some());
    }

    #[test]
    fn the_warning_names_the_total_the_threshold_and_the_three_largest() {
        let m = held(&[900, 800, 700, 600, 50], 2048);
        let w = m.warning().expect("3050 MB is over 2048 MB");
        assert_eq!(w, "idle panes hold 3.0 GB, over the 2.0 GB warning — largest: wsp-0 (900 MB), wsp-1 (800 MB), wsp-2 (700 MB)");
    }

    #[test]
    fn no_idle_panes_reads_as_none_and_never_warns() {
        let m = held(&[], 0);
        assert_eq!(m.line(), "idle panes: none");
        assert!(m.warning().is_none());
    }

    #[test]
    fn the_threshold_is_the_environment_then_the_file_then_the_default() {
        assert_eq!(warn_mb_from(None, None), DEFAULT_WARN_MB);
        assert_eq!(warn_mb_from(None, Some(r#"{"warn_mb": 512}"#)), 512);
        assert_eq!(warn_mb_from(Some("64"), Some(r#"{"warn_mb": 512}"#)), 64);
        assert_eq!(warn_mb_from(Some("nonsense"), Some(r#"{"warn_mb": 512}"#)), 512);
        assert_eq!(warn_mb_from(None, Some("not json")), DEFAULT_WARN_MB);
    }

    #[test]
    fn json_carries_the_total_the_verdict_and_each_pane() {
        let j = held(&[1500, 1000], 2048).json();
        assert_eq!(j["total_kb"], 2500 * MB);
        assert_eq!(j["over"], true);
        assert_eq!(j["panes"][0]["name"], "wsp-0");
    }
}
