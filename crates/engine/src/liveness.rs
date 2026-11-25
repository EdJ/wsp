//! Is there a daemon on this machine, and does it need one: the probes `wsp
//! doctor`, `wip`, a seat's brief and `wsp watch --status` ask, read from the
//! process list and the local backends. Moved down from [`crate::daemon`]
//! (wsp-435), which re-exports them, so the seats that ask need not reach up to
//! the daemon itself.

use std::path::{Path, PathBuf};

use crate::store::Store;

/// Is this `ps -E` line's *command* `wsp daemon`?
///
/// The command only. `ps -E` appends the environment to the command in one
/// field, and every agent's shell carries a `_=` or a `WSP_*` mentioning wsp —
/// so a match anywhere in the line would count panes as daemons. The command is
/// the words before the first `KEY=value`, which is where argv stops.
pub fn is_daemon(rest: &str) -> bool {
    is_wsp(rest, "daemon")
}

/// Is this `ps -E` line `wsp <sub>`? The same reading, for a subcommand that
/// is not the daemon.
pub fn is_wsp(rest: &str, sub: &str) -> bool {
    let mut words = argv(rest);
    // `-v` and anything after it are somebody's flags, not our business.
    is_exe(words.next()) && words.next() == Some(sub)
}

/// The words of that field that are argv: everything before the first
/// `KEY=value`, which is where the command stops and `ps -E`'s environment
/// begins.
pub fn argv(rest: &str) -> impl Iterator<Item = &str> {
    rest.split_whitespace().take_while(|w| !is_env_word(w))
}

/// Is that argv0 this binary, under whatever path it was started from?
pub fn is_exe(argv0: Option<&str>) -> bool {
    argv0.and_then(|a| a.rsplit('/').next()) == Some("wsp")
}

pub fn is_env_word(w: &str) -> bool {
    let Some((key, _)) = w.split_once('=') else { return false };
    !key.is_empty()
        && key.starts_with(|c: char| c.is_ascii_alphabetic() || c == '_')
        && key.chars().all(|c| c.is_ascii_alphanumeric() || c == '_')
}

/// Is it pointed at *this* store's state, the way [`Store::open`] would read it?
///
/// The one filter that matters: `wsp sandbox` runs a daemon of its own with
/// `WSP_STATE` set, and a check that called that a second daemon would cry wolf
/// on a machine where sandboxes are the normal way to test a change.
pub fn for_store(rest: &str, state: &Path) -> bool {
    state_of(rest) == state
}

/// The state directory a `ps -E` line's environment points at.
pub fn state_of(rest: &str) -> PathBuf {
    match rest.split_whitespace().find_map(|w| w.strip_prefix("WSP_STATE=")) {
        Some(v) => PathBuf::from(v),
        None => Store::default_state(),
    }
}

/// Every `wsp daemon` on this machine pointed at `state`, or `None` when the
/// process list could not be read at all — which no caller may read as "none",
/// because a machine with no processes on it is a machine that cannot answer.
pub fn running(state: &Path) -> Option<Vec<u32>> {
    let all = crate::host::Host::processes(&crate::host::LOCAL);
    if all.is_empty() {
        return None;
    }
    Some(daemons(&all, std::process::id(), state))
}

/// The daemons in a process list, never counting `me`.
///
/// Our own pid only, and not our ancestors. Ancestors were excluded once,
/// when a shell whose line carried `wsp daemon` could pass for one — and every
/// agent the tick spawns is a descendant of the daemon, so its brief was told
/// "no wsp daemon" by the one daemon on the machine (wsp-200). [`is_daemon`]
/// reads argv alone now, so a shell cannot match; the daemon asking about
/// others at its claim is the one case left, and `reload` keeps its pid.
pub fn daemons(all: &[(u32, u32, String)], me: u32, state: &Path) -> Vec<u32> {
    all.iter()
        .filter(|(pid, _, _)| *pid != me)
        .filter(|(_, _, rest)| is_daemon(rest) && for_store(rest, state))
        .map(|(pid, _, _)| *pid)
        .collect()
}

/// Has anything on this machine that wants a daemon answered?
///
/// The gate on [`loud`], and one function rather than four ad-hoc readings of
/// "is this machine doing anything".
///
/// It is asked of every local backend: one predicate, four call sites — and
/// four call sites that had drifted into three answers is the failure the doc
/// on [`loud`] claims not to have.
pub fn a_backend_answered() -> bool {
    crate::place::local_backends().iter().any(|b| b.census().is_ok())
}

/// The one sentence four surfaces show when this machine has no daemon.
///
/// **`wsp-145`, and why it is a function and not a paragraph.** The daemon was
/// started by a startup hook that stopped being on this machine, so
/// nothing was starting it — and the one surface that *did* notice was
/// `wsp doctor`, which nobody runs unless something is already wrong. For six
/// days the symptom was silence from a watchdog, and `wsp watch --status` said
/// `ticked 6d23h ago, the process is gone` without saying anything about *why*
/// the process was gone, which was the only question anybody had.
///
/// Four call sites, one predicate, and none of them is free of context cost,
/// so the reading is *narrow on purpose*:
///
/// - [`health`] is the report, and says it when a machine has a daemon to miss.
/// - `wip`'s heading is what a person reads first on a machine that looks
///   busy, which is precisely the machine whose wake spools are all holding.
/// - a brief's **seat** line, and only that line: the brief is re-read on every
///   request of every session, so an ordinary agent must still pay nothing.
///   A seat is the one reader for whom "nothing will wake me" is a fact about
///   its own job.
/// - the first line of `wsp watch --status`, which is the thing a governor runs
///   when it has noticed a silence and is deciding what the silence means.
///
/// `None` for `running` says nothing, for [`health`]'s reason: a `ps` that will
/// not answer is not an empty machine, and "no daemon running" would send
/// somebody to start a second one. `backend_up` is `health`'s own gate — a
/// machine with nothing to spawn onto wants no daemon, and is told
/// nothing.
pub fn loud(running: Option<&[u32]>, backend_up: bool) -> Option<String> {
    match running {
        Some([]) if backend_up => Some(
            "no wsp daemon — tokens, TTLs and the unattended pass are all off, and nothing will wake a seat (`wsp install` loads one)"
                .into(),
        ),
        _ => None,
    }
}

/// What `doctor` says about the daemon: that there is one, that there are two,
/// or that there is none.
///
/// The noticing half of t-260817-052, and the half that works on a machine
/// where nothing has been restarted since: the marker is only written by a
/// daemon new enough to write one, so `running` is what is asked, and the marker
/// is only used to say which of them is the one that claimed the store.
///
/// `None` for `running` says nothing at all, deliberately. A `ps` that will not
/// answer is not evidence of an empty machine, and "no daemon running" is a line
/// that would send somebody to start a second one.
pub fn health(
    holder: Option<u32>,
    running: Option<&[u32]>,
    backend_up: bool,
    problems: &mut Vec<String>,
    notes: &mut Vec<String>,
) {
    let Some(running) = running else { return };
    let list = |pids: &[u32]| {
        pids.iter().map(|p| p.to_string()).collect::<Vec<_>>().join(", ")
    };
    match running {
        [] => {
            // Only where it means something is missing, and in the one sentence
            // `wip`, a brief's seat line and `wsp watch --status` also use — so
            // a person who is told it in one place and finds it missing in the
            // next is not being given two truths, and `wsp-145` is not paid for
            // four times in wording.
            notes.extend(loud(Some(running), backend_up));
        }
        [one] => match holder == Some(*one) {
            true => notes.push(format!("daemon up, pid {one}")),
            // An image from before the marker existed, so nothing yet stops a
            // second one joining it. Not a problem — one daemon is the right
            // number — and nothing to do either: the reload path re-enters
            // `run`, so it claims the store the next time anybody installs.
            false => notes.push(format!(
                "daemon up as pid {one}, from before the single-daemon marker — it claims the store on its next reload or restart"
            )),
        },
        many => {
            problems.push(format!(
                "{} wsp daemons are running against this store (pids {}) — they run the cycle and the repairs twice against one store, and the loser of any race is silent",
                many.len(),
                list(many)
            ));
            let strays: Vec<u32> = many.iter().copied().filter(|p| Some(*p) != holder).collect();
            problems.push(match holder {
                Some(h) if many.contains(&h) => format!(
                    "  pid {h} is the one that claimed the store — `kill {}`",
                    list(&strays)
                ),
                _ => format!(
                    "  none of them claimed the store, so they all predate the guard — `kill` all but one, and the survivor claims it on its next start"
                ),
            });
        }
    }
}

/// How long `cycle.log` may be quiet before a machine with no daemon process
/// is told its fleet has stopped (wsp-449).
pub const SILENT_SECS: u64 = 60;

/// [`loud`], plus the two things a dropped launchd job leaves: the note an
/// install that could not load it keeps, and a `cycle.log` quiet for over a
/// minute. Both only when no daemon process is running — a log can be quiet
/// for good reasons while one is — so a live daemon is never called dead.
pub fn dead(state: &Path, running: Option<&[u32]>, backend_up: bool) -> Option<String> {
    if let Some(l) = loud(running, backend_up) {
        return Some(l);
    }
    if !matches!(running, Some([])) {
        return None;
    }
    if let Some(n) = crate::launchd::unloaded_note(state) {
        return Some(n);
    }
    let quiet = std::fs::metadata(state.join("cycle.log")).ok()?.modified().ok()?.elapsed().ok()?;
    (quiet.as_secs() > SILENT_SECS).then(|| {
        format!(
            "the daemon is not running — cycle.log has been silent for {} and no daemon process is up (`wsp install --daemon` loads it)",
            crate::util::duration_human(quiet.as_secs() as i64)
        )
    })
}
