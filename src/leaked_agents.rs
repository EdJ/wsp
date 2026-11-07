//! Agents running for a store no daemon keeps: `wsp-333`.
//!
//! On 2026-11-02 nine live `claude` processes were found hours after the rows
//! whose tests started them had finished, in throwaway stores that `wsp wip`
//! never lists. A sandbox store still spawns real processes, and nothing ended
//! them when the sandbox went.
//!
//! **A leak** is an agent in a seat (its environment carries
//! [`crate::base::seat::SEAT_ENV`]) whose store is not this daemon's and has no
//! daemon of its own. A sandbox with its daemon running is somebody's work and
//! is left alone; an agent with no `WSP_STATE` is the default store's.
//!
//! **Named, then ended after a grace period.** The first tick that sees a leak
//! says so in `cycle.log` with the time it will be ended; once the grace has
//! passed it is sent `TERM`, and `KILL` on a later tick if it is still there.
//! What was seen is kept in `leaked-agents.json` so a restarted daemon does not
//! start the grace again.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use serde_json::{json, Value};

use crate::store::Store;
use crate::util;

const FILE: &str = "leaked-agents.json";

/// How long a leak is named before it is ended.
pub(crate) const GRACE: i64 = 30 * 60;

const KINDS: &[&str] = &["claude", "opencode", "pi"];

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Leak {
    pub pid: u32,
    pub kind: String,
    pub seat: String,
    pub state: PathBuf,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Seen {
    pub first: i64,
    pub termed: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum Act {
    Name(Leak),
    Term(Leak),
    Kill(Leak),
}

fn kind_of(rest: &str) -> Option<&'static str> {
    let words: Vec<&str> = crate::daemon::argv(rest).take(2).collect();
    words.iter().find_map(|w| {
        let name = w.rsplit('/').next()?;
        KINDS.iter().find(|k| **k == name).copied()
    })
}

fn seat_of(rest: &str) -> Option<&str> {
    rest.split_whitespace().find_map(|w| w.strip_prefix(&format!("{}=", crate::base::seat::SEAT_ENV)))
}

/// The agents in a `ps -E` table that are in a seat of a store that is not
/// `ours` and that `kept` says nobody keeps. Nothing in `spare` is ever one.
pub(crate) fn find(all: &[(u32, u32, String)], ours: &Path, spare: &[u32], kept: &dyn Fn(&Path) -> bool) -> Vec<Leak> {
    let mut out = Vec::new();
    for (pid, _, rest) in all {
        let (Some(kind), Some(seat)) = (kind_of(rest), seat_of(rest)) else { continue };
        let state = crate::daemon::state_of(rest);
        if state == ours || spare.contains(pid) || kept(&state) {
            continue;
        }
        out.push(Leak { pid: *pid, kind: kind.into(), seat: seat.into(), state });
    }
    out
}

/// What to do about `leaks` at `now`, given what was seen before. `seen` is
/// left as the next tick should find it: only the leaks still there.
pub(crate) fn judge(seen: &mut BTreeMap<u32, Seen>, leaks: &[Leak], now: i64) -> Vec<Act> {
    seen.retain(|pid, _| leaks.iter().any(|l| l.pid == *pid));
    let mut acts = Vec::new();
    for leak in leaks {
        match seen.get_mut(&leak.pid) {
            None => {
                seen.insert(leak.pid, Seen { first: now, termed: false });
                acts.push(Act::Name(leak.clone()));
            }
            Some(s) if s.termed => acts.push(Act::Kill(leak.clone())),
            Some(s) if now - s.first >= GRACE => {
                s.termed = true;
                acts.push(Act::Term(leak.clone()));
            }
            Some(_) => {}
        }
    }
    acts
}

pub(crate) fn sentence(act: &Act) -> String {
    let (leak, what) = match act {
        Act::Name(l) => (
            l,
            format!(
                "no daemon keeps that store; ending it in {} min unless one starts",
                GRACE / 60
            ),
        ),
        Act::Term(l) => (l, "no daemon kept that store for the grace; ended it".to_string()),
        Act::Kill(l) => (l, "still running after the end; killed it".to_string()),
    };
    format!(
        "leaked agent: {} pid {} in seat {} of {}: {what}",
        leak.kind,
        leak.pid,
        leak.seat,
        leak.state.display()
    )
}

fn signal(pid: u32, sig: &str) {
    let _ = std::process::Command::new("kill")
        .arg(format!("-{sig}"))
        .arg(pid.to_string())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status();
    crate::place_super::signal_group(pid, sig);
}

fn read(store: &Store) -> BTreeMap<u32, Seen> {
    let value: Value = std::fs::read_to_string(store.state_file(FILE)).ok().and_then(|s| serde_json::from_str(&s).ok()).unwrap_or(json!({}));
    value
        .as_object()
        .into_iter()
        .flatten()
        .filter_map(|(pid, v)| {
            let seen = Seen { first: v["first"].as_i64()?, termed: v["termed"].as_bool().unwrap_or(false) };
            Some((pid.parse().ok()?, seen))
        })
        .collect()
}

fn write(store: &Store, seen: &BTreeMap<u32, Seen>) {
    let value: serde_json::Map<String, Value> =
        seen.iter().map(|(pid, s)| (pid.to_string(), json!({ "first": s.first, "termed": s.termed }))).collect();
    let _ = std::fs::write(store.state_file(FILE), Value::Object(value).to_string());
}

/// One pass of the daemon's tick. Not under test, for `disk::tidy`'s reason;
/// the tests drive [`find`] and [`judge`].
pub(crate) fn tidy(store: &Store) {
    if cfg!(test) {
        return;
    }
    let all = crate::cmd_sandbox::processes();
    if all.is_empty() {
        return;
    }
    let spare = crate::cmd_sandbox::ancestors(&all);
    let kept = |state: &Path| crate::daemon::running(state).is_none_or(|d| !d.is_empty());
    let leaks = find(&all, &store.state, &spare, &kept);
    let mut seen = read(store);
    if leaks.is_empty() && seen.is_empty() {
        return;
    }
    for act in judge(&mut seen, &leaks, util::epoch_secs()) {
        crate::store::log_line(store, &sentence(&act));
        match act {
            Act::Name(_) => {}
            Act::Term(l) => signal(l.pid, "TERM"),
            Act::Kill(l) => signal(l.pid, "KILL"),
        }
    }
    write(store, &seen);
}

#[cfg(test)]
mod tests {
    use super::*;

    fn line(pid: u32, command: &str, env: &str) -> (u32, u32, String) {
        (pid, 1, format!("{command} {env}"))
    }

    fn table() -> Vec<(u32, u32, String)> {
        vec![
            line(10, "claude --model x", "HOME=/h WSP_SEAT_ID=cpd-1 WSP_STATE=/tmp/e2e/state"),
            line(11, "/usr/local/bin/claude", "WSP_SEAT_ID=cpd-2 WSP_STATE=/tmp/sb/state"),
            line(12, "claude", "WSP_SEAT_ID=cpd-3"),
            line(13, "claude", "WSP_SEAT_ID=cpd-4 WSP_STATE=/ours"),
            line(14, "claude", "HOME=/h"),
            line(15, "vim notes", "WSP_SEAT_ID=cpd-5 WSP_STATE=/tmp/e2e/state"),
            line(16, "node /x/bin/claude", "WSP_SEAT_ID=cpd-6 WSP_STATE=/tmp/e2e/state"),
        ]
    }

    #[test]
    fn an_agent_in_a_seat_of_a_store_nobody_keeps_is_a_leak() {
        let kept = |s: &Path| s == Path::new("/tmp/sb/state") || s == Store::default_state();
        let found = find(&table(), Path::new("/ours"), &[], &kept);
        let pids: Vec<u32> = found.iter().map(|l| l.pid).collect();
        assert_eq!(pids, vec![10, 16], "{found:?}");
        assert_eq!(found[0].seat, "cpd-1");
        assert_eq!(found[0].state, PathBuf::from("/tmp/e2e/state"));
    }

    #[test]
    fn a_store_with_no_state_is_the_default_one_and_keeps_its_agents_by_its_daemon() {
        let default = Store::default_state();
        let all = vec![line(12, "claude", "WSP_SEAT_ID=cpd-3")];
        assert!(find(&all, &default, &[], &|_| false).is_empty(), "ours");
        assert_eq!(find(&all, Path::new("/sandbox/state"), &[], &|_| false).len(), 1, "a sandbox daemon does not know it");
        assert!(find(&all, Path::new("/sandbox/state"), &[], &|s| s == default).is_empty(), "the default store's daemon keeps it");
    }

    #[test]
    fn what_this_process_stands_on_is_never_a_leak() {
        let found = find(&table(), Path::new("/ours"), &[10, 16], &|_| false);
        let pids: Vec<u32> = found.iter().map(|l| l.pid).collect();
        assert_eq!(pids, vec![11, 12], "10 and 16 are spared; 13 is ours; 14 and 15 are not agents in a seat");
    }

    fn leak(pid: u32) -> Leak {
        Leak { pid, kind: "claude".into(), seat: "cpd-1".into(), state: "/tmp/e2e/state".into() }
    }

    #[test]
    fn a_leak_is_named_when_first_seen_and_ended_only_after_the_grace() {
        let mut seen = BTreeMap::new();
        let leaks = [leak(10)];
        assert_eq!(judge(&mut seen, &leaks, 1000), vec![Act::Name(leak(10))]);
        assert!(judge(&mut seen, &leaks, 1000 + GRACE - 1).is_empty(), "named once, not every tick");
        assert_eq!(judge(&mut seen, &leaks, 1000 + GRACE), vec![Act::Term(leak(10))]);
        assert_eq!(judge(&mut seen, &leaks, 1000 + GRACE + 60), vec![Act::Kill(leak(10))], "still there after TERM");
    }

    #[test]
    fn a_leak_that_is_gone_or_kept_again_starts_its_grace_over() {
        let mut seen = BTreeMap::new();
        judge(&mut seen, &[leak(10)], 0);
        assert!(judge(&mut seen, &[], 100).is_empty());
        assert!(seen.is_empty());
        assert_eq!(judge(&mut seen, &[leak(10)], GRACE + 200), vec![Act::Name(leak(10))]);
    }

    #[test]
    fn the_line_names_the_agent_its_seat_and_its_store() {
        let said = sentence(&Act::Name(leak(10)));
        for part in ["claude", "pid 10", "cpd-1", "/tmp/e2e/state", "30 min"] {
            assert!(said.contains(part), "{part} not in {said}");
        }
    }

    #[test]
    fn what_was_seen_survives_a_restart() {
        let iso = util::isolated("leaked-agents");
        let store = Store::open();
        let mut seen = BTreeMap::new();
        judge(&mut seen, &[leak(10)], 5);
        write(&store, &seen);
        assert_eq!(read(&store), seen);
        drop(iso);
    }
}
