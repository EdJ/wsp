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
    let seats: Vec<(String, String)> = agents
        .iter()
        .filter(|a| a.state == State::Idle)
        .map(|a| {
            let seat = a.seat.as_str().to_string();
            let task = bindings.get(&seat).and_then(|b| b.get("task_id")).and_then(Value::as_str).filter(|t| !t.is_empty());
            let name = task.map(str::to_string).or_else(|| Some(a.label.clone()).filter(|l| !l.is_empty())).unwrap_or_else(|| seat.clone());
            (seat, name)
        })
        .collect();
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
    match ps() {
        Some(ps) => of(&ps, &seats, &roots, warn_kb),
        None => IdleMemory { warn_kb, unread: seats.len(), ..IdleMemory::default() },
    }
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

#[cfg(test)]
mod tests {
    use super::*;

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
