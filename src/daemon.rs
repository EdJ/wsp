//! The only long-lived piece: herdr events + store polling -> token refresh.
//!
//! "The only" is now enforced rather than assumed — one daemon per store, and
//! the argument for how a second one is turned away is under *one daemon per
//! store* below.
//!
//! Deliberately dependency-free. One thread blocks on the event stream and
//! feeds a channel; the main loop coalesces bursts (herdr emits
//! `workspace_focused` on every sidebar hover) and refreshes TTLs on a timer.
//!
//! It carries one passenger, and being the only long-lived process is the whole
//! qualification for it: [`crate::attention`] derives what is wrong on this
//! machine once a minute and puts the changes where a hook can see them. That
//! is the one thing in wsp that happens at nobody's request — every other
//! surface states the same facts to whoever is looking. It rides above the
//! fingerprint gate below rather than beside the sync, because a stopped agent
//! changes nothing in the store and raises no event this daemon can subscribe
//! to, so a pass that ran only when something woke us would never run on the
//! one condition it is for.

use std::path::Path;
use std::sync::mpsc::{self, RecvTimeoutError};
use std::time::{Duration, Instant};

use crate::attention;
use crate::herdr;
use crate::store::Store;
use crate::sync::{self, Cache};
use crate::tunnel::Supervisor;

const DEBOUNCE: Duration = Duration::from_millis(400);
const TICK: Duration = Duration::from_secs(20);
const REFRESH: Duration = Duration::from_secs(15 * 60);

/// What is worth a re-sync. `pane.agent_status_changed` is not here and must
/// not be: it is per-pane and its request requires a `pane_id`, so one entry
/// asking for it globally refuses the entire list — see [`crate::herdr`]. The
/// daemon has a 20s tick to fall back on, which is why this went unnoticed for
/// as long as it did. `pane.agent_detected` is what a new agent raises.
const EVENTS: &[&str] = &[
    "workspace.created",
    "workspace.closed",
    "workspace.renamed",
    "workspace.focused",
    "tab.created",
    "tab.closed",
    "pane.created",
    "pane.closed",
    "pane.exited",
    "pane.agent_detected",
    "worktree.created",
    "worktree.opened",
];

/// The stamp we are now running under, if it is not the one we started with.
///
/// `None` for `started_as` is a daemon that could not read its own path at
/// startup, and `None` from a later reading is one that cannot read it now —
/// mid-install, most likely. Neither is a reason to replace ourselves with
/// something we cannot describe, so both answer "no change".
///
/// Takes the reading rather than doing it, so the decision can be tested
/// without a filesystem — the reading itself is pinned in [`crate::util`].
fn changed(started_as: Option<(u64, u64)>, now: Option<(u64, u64)>) -> Option<(u64, u64)> {
    match (started_as, now) {
        (Some(was), Some(now)) if was != now => Some(now),
        _ => None,
    }
}

/// Replace this process with the binary now on disk. Returns only on failure.
///
/// `wsp panel` and `wsp view` have done this since the day they were written,
/// and the daemon was the one long-lived process left out — so an
/// `install -m 755 target/release/wsp ~/.local/bin/wsp` reached every pane on
/// the machine and left the daemon executing whatever it was started with,
/// indefinitely. The one running when this was written had been up for a day
/// and had sat through two installs. Nothing said so: it answers, it syncs, it
/// is simply doing it with old code, and the store fix you just shipped is
/// live in the panels and absent from the process that polls hardest.
///
/// `exec` rather than spawn-and-exit, for the reason the panel does it: the
/// process id, and so herdr's idea of what it started, survives. What does not
/// survive is the event-reader thread and its socket — which costs nothing,
/// because the new image resubscribes on the way up, and a dropped stream is
/// the case that code already handles on every herdr restart. Nothing is owed
/// to the sync either: this is called at the top of the loop, where the last
/// one has finished and the next has not begun.
fn reload(verbose: bool) -> std::io::Error {
    use std::os::unix::process::CommandExt;
    let exe = match std::env::current_exe() {
        Ok(e) => e,
        Err(e) => return e,
    };
    let mut c = std::process::Command::new(exe);
    c.arg("daemon");
    // Carried across deliberately: a daemon that went quiet the first time it
    // reloaded would look exactly like one that had died.
    if verbose {
        c.arg("-v");
    }
    c.exec()
}


/// Machines we already have an event reader thread for. Each thread puts its
/// own name in and takes it out again on the way past, so the tick can tell
/// "already watching" from "needs watching" without keeping a handle.
type Readers = std::sync::Arc<std::sync::Mutex<std::collections::BTreeSet<String>>>;

/// Start an event reader for every reachable machine that has not got one.
///
/// Started here rather than by the supervisor because the supervisor holds
/// connections and this holds a subscription to the far end of one: it is only
/// worth starting once the tunnel is up and answering, which is precisely what
/// the supervisor has just decided.
///
/// The thread owns its own lifetime. It re-subscribes when a stream ends — a
/// far herdr restarting, or a tunnel dropping — and gives up only when the
/// machine has gone from the store or been retired, which is the one condition
/// under which nobody wants it back.
fn watch_machines(
    tunnels: &Supervisor,
    readers: &Readers,
    tx: &mpsc::Sender<()>,
    verbose: bool,
) {
    for name in tunnels.reachable() {
        {
            let mut held = readers.lock().unwrap_or_else(|e| e.into_inner());
            if !held.insert(name.clone()) {
                continue;
            }
        }
        if verbose {
            eprintln!("wsp daemon: watching {name}");
        }
        let readers = readers.clone();
        let tx = tx.clone();
        std::thread::spawn(move || {
            loop {
                // Retired or removed is the only way out. Unreachable is not:
                // the machine is expected back, and giving up on it here would
                // leave it silent until something else happened to restart us.
                let store = Store::open();
                if !store.machine(&name).map(|m| m.is_active()).unwrap_or(false) {
                    break;
                }
                let tx2 = tx.clone();
                let res =
                    herdr::subscribe_on(Some(&name), EVENTS, move |_e, _d| tx2.send(()).is_ok());
                std::thread::sleep(match res {
                    Err(_) => Duration::from_secs(5),
                    Ok(()) => Duration::from_millis(500),
                });
            }
            readers.lock().unwrap_or_else(|e| e.into_inner()).remove(&name);
        });
    }
}

/// Why this daemon must not run: it is pointed at a herdr that is not the
/// machine's, and at a store nobody named.
///
/// The third defence in robustness-021, and the one that holds when the other two
/// are got right and something starts a daemon anyway. A herdr session server
/// starts every enabled plugin's `[[startup]]` command — `plugins.json` is
/// global, not per session — with its own environment. So a sandbox whose
/// server was started carelessly gets a daemon holding *its* socket and the
/// *live* store, and the first thing a daemon does is sync: `herdr::panes()`
/// answers with nothing, and `reap_bindings` takes every binding in the real
/// store with it. One line of environment between a test instance and everyone
/// else's claims.
///
/// The rule is narrow on purpose. A daemon on the default socket is the
/// ordinary one and is never refused, whatever its store. Only a daemon that is
/// somewhere else has to say which store it is for — and it has to say *both*
/// halves, because `WSP_HOME` alone leaves state at `~/.local/state/wsp`, which
/// is where bindings and claims actually live (robustness-011).
///
/// # The socket decides, and `HERDR_SESSION` deliberately does not
///
/// There are two signals for "this is not the machine's herdr" and only one of
/// them is ours. The socket path is a fact wsp can observe: this is where I am
/// pointed, that is where the default is, they differ. `HERDR_SESSION` is
/// herdr's own vocabulary — herdr calls the main session `default`, and if a
/// server ever exported `HERDR_SESSION=default` to its `[[startup]]` plugins,
/// a predicate that read it would refuse the *real* daemon on every herdr
/// restart, and nobody would notice until claims stopped syncing. It does not
/// today: neither the live daemon nor any live pane carries the variable, while
/// a `--session` pane does.
///
/// So it stays in the message, where `herdr session `wsp-w1`` beats a path for
/// telling you what you are looking at, and out of the predicate. The general
/// rule, which is the argument of robustness-017: given two signals for the same
/// question, prefer the one that survives the backend changing. Anything that
/// is herdr's naming convention rather than an observable fact is something
/// herdr can change without telling us, and something a different backend would
/// not have at all — so the next person reading this should not re-add the
/// session check as an obvious improvement.
fn misdirected(
    session: Option<&str>,
    socket: Option<&str>,
    home: Option<&str>,
    state: Option<&str>,
) -> Option<String> {
    let default_socket = crate::util::home().join(".config/herdr/herdr.sock");
    let elsewhere = socket
        .map(|s| !s.trim().is_empty() && std::path::Path::new(s) != default_socket)
        .unwrap_or(false);
    if !elsewhere {
        return None;
    }
    let told = |v: Option<&str>| v.map(|s| !s.trim().is_empty()).unwrap_or(false);
    if told(home) && told(state) {
        return None;
    }
    let which = match session {
        Some(s) if !s.trim().is_empty() => format!("herdr session `{}`", s.trim()),
        _ => format!("the herdr at {}", socket.unwrap_or("")),
    };
    Some(format!(
        "refusing to run against {}: it is not this machine's herdr, and no WSP_HOME/WSP_STATE says which store it is for.\n\
         wsp: a daemon on another herdr and the live store reaps every binding in it on its first sync.\n\
         wsp: set both, or start the session with `wsp sandbox`, which does.",
        which
    ))
}

// ---- one daemon per store ---------------------------------------------------
//
// Two of these ran for forty-two hours and nothing noticed (t-260817-052). One
// was started by hand on the 15th with `-v`, survived herdr's death still
// retrying its socket, and was still there when herdr came back and its
// `[[startup]]` hook started a second. So the trigger is not carelessness: it is
// any crash or restart, and the hand-started daemon that outlives the terminal
// it was typed in is the ordinary way to get one.
//
// Both then do everything twice against one store and one herdr: push tokens,
// refresh TTLs, and reap. `sync` reaps bindings against the pane list herdr
// answers with, and render-040 is the record of what one wrong reap costs.
// The loser of any race is silent, which is why forty-two hours passed.
//
// # Refuse, not take over
//
// The case that will actually happen is a marker a crash left behind, and that
// case is not a contest at all — the pid in it is not there any more, and the
// answer is to take it. Once liveness is checked, the only case left is a
// *verified live* daemon on this store, and for that one there is no scenario
// where the newcomer is the more correct process:
//
// - A newcomer pointed at the wrong herdr or an unnamed store is already
//   refused, earlier and for a sharper reason — see `misdirected`.
// - An incumbent running old code already replaces itself; see `reload`.
//
// What taking over would add is killing a working process on the strength of a
// file, and one real regression: a person who types `wsp daemon -v` to watch it
// for a minute would displace the machine's daemon and leave nothing behind when
// they hit Ctrl-C. So the newcomer refuses and says which pid holds it, which is
// also the house answer everywhere else — `wsp claim` refuses a task a live
// agent holds.
//
// It refuses only on *positive* evidence, though. An unreadable process list, or
// a pid that is alive but is not a wsp daemon (numbers get reused), leaves the
// marker no better than empty — and refusing on a marker we cannot corroborate
// is how a machine ends up with no daemon at all and one line in a log nobody
// reads. The failure this fixes is two daemons; the failure it must not create
// is zero.

/// What a starting daemon does about whoever the marker names.
#[derive(Debug, PartialEq)]
enum Claim {
    /// Take the marker. Carries the pid being displaced in it, if there was
    /// one, because "a daemon died here on Saturday" is worth one line.
    Take(Option<u32>),
    /// It already names us, which is what an exec-`reload` finds: the pid
    /// survives the exec, so the new image must not read its own marker as a
    /// rival and refuse — that would end the machine's daemon on the next
    /// `wsp install`, quietly, which is a worse bug than the one being fixed.
    Keep,
    /// A daemon for this store is running and answering. Say which.
    Refuse(u32),
}

/// `live` is every `wsp daemon` for this store that is actually running, or
/// `None` when we could not find out. Passed in rather than looked up, so the
/// decision is testable without a machine that has daemons on it.
fn claim(holder: Option<u32>, me: u32, live: Option<&[u32]>) -> Claim {
    match holder {
        Some(h) if h == me => Claim::Keep,
        Some(h) if live.is_some_and(|l| l.contains(&h)) => Claim::Refuse(h),
        Some(h) => Claim::Take(Some(h)),
        None => Claim::Take(None),
    }
}

/// Where a daemon already running stands, as of the marker.
#[derive(Debug, PartialEq)]
enum Standing {
    /// The marker is ours. The ordinary answer, every tick.
    Ours,
    /// Somebody else holds it now. Two daemons start in the same instant, both
    /// read an empty marker, both write, and one of them is this: the one that
    /// is not in the marker stands down, so the invariant holds even when the
    /// start-time check could not see the other coming.
    Lost(u32),
    /// Nothing holds it. The state directory went away under us — a `wsp init`,
    /// a sandbox teardown — and an unmarked daemon is one `doctor` would go on
    /// reporting as a stray for ever. Claim it again.
    Unmarked,
}

fn standing(holder: Option<u32>, me: u32) -> Standing {
    match holder {
        Some(h) if h == me => Standing::Ours,
        Some(h) => Standing::Lost(h),
        None => Standing::Unmarked,
    }
}

/// Is this `ps -E` line's *command* `wsp daemon`?
///
/// The command only. `ps -E` appends the environment to the command in one
/// field, and every agent's shell carries a `_=` or a `WSP_*` mentioning wsp —
/// so a match anywhere in the line would count panes as daemons. The command is
/// the words before the first `KEY=value`, which is where argv stops.
fn is_daemon(rest: &str) -> bool {
    is_wsp(rest, "daemon")
}

/// Is this `ps -E` line `wsp <sub>`? The same reading, for a subcommand that
/// is not the daemon — see [`surface_drawing`], which is the other long-lived
/// wsp process on a machine.
fn is_wsp(rest: &str, sub: &str) -> bool {
    let mut words = argv(rest);
    // `-v` and anything after it are somebody's flags, not our business.
    is_exe(words.next()) && words.next() == Some(sub)
}

/// The words of that field that are argv: everything before the first
/// `KEY=value`, which is where the command stops and `ps -E`'s environment
/// begins.
fn argv(rest: &str) -> impl Iterator<Item = &str> {
    rest.split_whitespace().take_while(|w| !is_env_word(w))
}

/// Is that argv0 this binary, under whatever path it was started from?
fn is_exe(argv0: Option<&str>) -> bool {
    argv0.and_then(|a| a.rsplit('/').next()) == Some("wsp")
}

fn is_env_word(w: &str) -> bool {
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
fn for_store(rest: &str, state: &Path) -> bool {
    match rest.split_whitespace().find_map(|w| w.strip_prefix("WSP_STATE=")) {
        Some(v) => Path::new(v) == state,
        None => state == Store::default_state(),
    }
}

/// Every `wsp daemon` on this machine pointed at `state`, or `None` when the
/// process list could not be read at all — which no caller may read as "none",
/// because a machine with no processes on it is a machine that cannot answer.
pub(crate) fn running(state: &Path) -> Option<Vec<u32>> {
    let all = crate::cmd_sandbox::processes();
    if all.is_empty() {
        return None;
    }
    // Never ourselves or anything we are standing on: `wsp doctor` run from a
    // shell has that shell above it, and the shell's line carries our command.
    let mine = crate::cmd_sandbox::ancestors(&all);
    Some(
        all.iter()
            .filter(|(pid, _, _)| !mine.contains(pid))
            .filter(|(_, _, rest)| is_daemon(rest) && for_store(rest, state))
            .map(|(pid, _, _)| *pid)
            .collect(),
    )
}

/// Is a `wsp surface` drawing this store's sidebar?
///
/// The question `panel install` and its auto-install have to ask, and the
/// answer that keeps the two surfaces from running at once: a surface *is* the
/// panel on that screen, so splitting a second one into a pane is the old way
/// arriving beside the new one — and it arrives by stealing the screen, which
/// is `fork-002`.
///
/// Asked of the process list rather than of a marker wsp writes, because the
/// fact wanted is "is one running now", and a file saying so outlives the
/// process that wrote it — through a herdr that was killed, a crash, or a fork
/// somebody has stopped running. There is nothing to go stale here.
///
/// Ancestors are **not** excluded, where [`running`] excludes them: the caller
/// is usually a `wsp` the surface itself started — the plugin hook under a
/// workspace the sidebar's `O` created — and the surface being our own parent
/// is the case this exists to catch rather than the case to filter out.
///
/// A `ps` that will not answer reads as "no surface", which is the same way
/// round as before this existed: an unanswerable process list should not stop
/// a panel being installed on a machine that has no fork on it at all.
pub(crate) fn surface_drawing(state: &Path) -> bool {
    crate::cmd_sandbox::processes()
        .iter()
        .any(|(_, _, rest)| is_wsp(rest, "surface") && for_store(rest, state))
}

/// The panes of this machine that still have a wsp of this store's running in
/// them, or `None` when the process list could not be read at all.
///
/// The question behind it is "is this pane still a panel, or only the shape one
/// left" — a husk, in `render-074`'s words. herdr restores a *layout* across a
/// restart, so a pane labelled `wsp` outlives the panel that was in it, and
/// there is nothing in herdr's answer to tell the two apart: the label is the
/// same, the tab is the same, and the pane is the same size it always was.
///
/// A running process is the fact that does distinguish them, and the panel puts
/// one in the pane it owns — `install_one` `exec`s over the shell, and a reload
/// `exec`s again over itself, so from the pane's first frame to its last there
/// is a `wsp` there with `HERDR_PANE_ID` in its environment. `ps -E` prints
/// that environment beside the command, which is what makes this answerable
/// without asking herdr anything it does not know.
///
/// **`None` is not "no panes".** The one caller closes panes on the strength of
/// this, and a `ps` that would not answer must never read as "nothing is
/// running anywhere" — that is a list of every panel on the machine to close.
/// Same rule, same reason, as [`running`].
pub(crate) fn wsp_panes(state: &Path) -> Option<std::collections::HashSet<String>> {
    let all = crate::cmd_sandbox::processes();
    if all.is_empty() {
        return None;
    }
    Some(manned(&all, state))
}

/// That reading of a process list, without the list. Any subcommand, not just
/// `panel`: the detail pane, the board and the fullscreen tree are ours too and
/// are drawn by other words, and what is being asked is whether *anything* of
/// ours is alive in there.
fn manned(all: &[(u32, u32, String)], state: &Path) -> std::collections::HashSet<String> {
    all.iter()
        .filter(|(_, _, rest)| is_exe(argv(rest).next()) && for_store(rest, state))
        .filter_map(|(_, _, rest)| pane_of(rest))
        .collect()
}

/// The pane a process is running in, from the environment `ps -E` appends.
fn pane_of(rest: &str) -> Option<String> {
    rest.split_whitespace()
        .find_map(|w| w.strip_prefix("HERDR_PANE_ID="))
        .filter(|v| !v.is_empty())
        .map(str::to_string)
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
pub(crate) fn health(
    holder: Option<u32>,
    running: Option<&[u32]>,
    herdr_up: bool,
    problems: &mut Vec<String>,
    notes: &mut Vec<String>,
) {
    let Some(running) = running else { return };
    let list = |pids: &[u32]| {
        pids.iter().map(|p| p.to_string()).collect::<Vec<_>>().join(", ")
    };
    match running {
        [] => {
            // Only where it means something is missing. A machine with no herdr
            // is a machine wsp works on and does not need a daemon for, and
            // `herdr_health` has already said so in its own words.
            if herdr_up {
                notes.push(
                    "no wsp daemon running — tokens and TTLs will not refresh (`wsp daemon`)".into(),
                );
            }
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
                "{} wsp daemons are running against this store (pids {}) — they push tokens, refresh TTLs and reap bindings twice against one herdr, and the loser of any race is silent",
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

pub fn run(store: &Store, verbose: bool) -> i32 {
    if let Some(why) = misdirected(
        std::env::var("HERDR_SESSION").ok().as_deref(),
        std::env::var("HERDR_SOCKET_PATH").ok().as_deref(),
        std::env::var("WSP_HOME").ok().as_deref(),
        std::env::var("WSP_STATE").ok().as_deref(),
    ) {
        eprintln!("wsp: {why}");
        return 2;
    }
    if !herdr::available() {
        eprintln!("wsp: no herdr socket at {}", herdr::socket_path().display());
        return 1;
    }

    // Third gate, and after the other two on purpose: a daemon that is going to
    // refuse for a sharper reason should never have written itself into the
    // marker, and one with no herdr to talk to is not the daemon this store
    // wants to be holding it.
    let me = std::process::id();
    let holder = store.daemon_holder();
    match claim(holder.as_ref().map(|(p, _)| *p), me, running(&store.state).as_deref()) {
        Claim::Refuse(pid) => {
            let up = match holder.as_ref().map(|(_, since)| since.as_str()) {
                Some(s) if !s.is_empty() => {
                    format!(", up {}", crate::util::duration_human(crate::util::since(s)))
                }
                _ => String::new(),
            };
            eprintln!("wsp: a wsp daemon for this store is already running as pid {pid}{up}");
            eprintln!(
                "wsp: two of them reap and refresh twice against one herdr — `kill {pid}` first if this one is meant to replace it"
            );
            // Not a failure. What the caller wanted — a daemon on this store —
            // is what is running, and herdr's `[[startup]]` hook gets a plugin
            // that did its job on every restart rather than one that looks
            // broken on all but the first.
            return 0;
        }
        Claim::Take(displaced) => {
            if let (Some(pid), Some((_, since))) = (displaced, holder.as_ref()) {
                eprintln!(
                    "wsp daemon: taking over from pid {pid}, which is gone{}",
                    match since.is_empty() {
                        true => String::new(),
                        false => format!(" (it claimed this store {since})"),
                    }
                );
            }
            store.set_daemon_holder(me);
        }
        // An exec-`reload`: same pid, same marker, nothing to write.
        Claim::Keep => {}
    }

    let (tx, rx) = mpsc::channel::<()>();
    let far = tx.clone();

    // Event reader. If the server restarts the stream ends; we retry rather
    // than exiting, because herdr restarts this daemon only on its own restart.
    std::thread::spawn(move || loop {
        let tx2 = tx.clone();
        let res = herdr::subscribe(EVENTS, move |_event, _data| tx2.send(()).is_ok());
        if res.is_err() {
            std::thread::sleep(Duration::from_secs(2));
        } else {
            std::thread::sleep(Duration::from_millis(500));
        }
    });

    // One more reader per executor, feeding the same channel. Which machine
    // woke us does not matter — only that something did — so the join is the
    // channel, exactly where it already was.
    let readers: Readers = Default::default();

    let mut cache = Cache::default();
    // Every executor's herdr socket, held open on this machine. Inert until
    // somebody runs `wsp machine add`; see `crate::tunnel`.
    let mut tunnels = Supervisor::new();
    let mut last_refresh = Instant::now();
    let mut last_fingerprint = store.fingerprint();
    // The half of this process that looks rather than syncs. Its ledger lives
    // in the store and not in this variable, so the `exec` below costs it
    // nothing; see `attention::load`.
    let mut pass = attention::Pass::new();
    // What we are executing, so we can notice an `install` landing underneath
    // us. See `reload` at the top of the loop for why the daemon needs this at
    // all when herdr restarts it.
    let mut started_as = crate::util::exe_stamp();

    // Before the first sync: herdr has just come back with its workspaces and
    // agent sessions restored, but pane ids are new and no binding survived.
    // Claims did, so rebuild from those.
    // Never reaping: a session being restored is a world that is briefly
    // missing workspaces, and this runs at exactly that moment.
    let r = crate::cmd_agent::reconcile(store, false);
    let live: Vec<String> = store.tasks().into_iter().map(|t| t.id).collect();
    let dropped = store.reap_claims(&live) + store.reap_worked(&live);
    if verbose && (r.bound > 0 || r.named > 0 || dropped > 0) {
        eprintln!(
            "wsp daemon: reconciled {} binding(s), named {} pane(s), dropped {dropped} stale claim(s)",
            r.bound, r.named
        );
    }

    // The agents that were running before the restart, offered back to whoever
    // is at the machine.
    //
    // **Before the first sync, and that ordering is the whole of it.** `sync`
    // rewrites the roster from what is running now — which, one second after
    // herdr came up, is nothing. Reading it afterwards would find an empty
    // census every time and offer nothing, on the one path this exists for.
    // So the copy is taken here, while the file on disk is still the census
    // from before the restart, and `cmd_resume::offered` reads that copy from
    // then on.
    //
    // Nothing is started. It opens one terminal with the question in it, and a
    // person answers. Ed, 2026-08-18: "it is not automatic: on load, ask the
    // user."
    crate::cmd_resume::ask_on_startup(store);

    match sync::sync(store, &mut cache, true) {
        Ok(r) => {
            if verbose {
                eprintln!("wsp daemon: initial sync — {} workspaces, {} panes", r.workspaces, r.panes);
            }
        }
        Err(e) => eprintln!("wsp daemon: initial sync failed: {e}"),
    }

    loop {
        let woken = match rx.recv_timeout(TICK) {
            Ok(()) => true,
            Err(RecvTimeoutError::Timeout) => false,
            Err(RecvTimeoutError::Disconnected) => return 0,
        };

        if woken {
            // Coalesce the burst.
            std::thread::sleep(DEBOUNCE);
            while rx.try_recv().is_ok() {}
        }

        // Both of the next two checks are here, at the top, because it is the
        // one point in the loop where nothing is half-done: no sync in flight,
        // no store lock held, the debounce already drained. Everything below
        // this line either finishes or is not started.
        //
        // Standing down comes before reloading: a daemon that is no longer the
        // one for this store should end, not exec a fresh image of itself into
        // the same argument.
        match standing(store.daemon_holder().map(|(p, _)| p), me) {
            Standing::Ours => {}
            Standing::Unmarked => store.set_daemon_holder(me),
            Standing::Lost(other) => {
                // The tunnels for the same reason the reload path shuts them
                // down: an `ssh -L` we leave behind holds the socket the daemon
                // that replaced us needs to bind.
                tunnels.shutdown();
                // And the claim to be watching, for the same reason as the
                // tunnels: a register left saying a reporter is running would
                // have `doctor` report the ticks stopping as a fault rather
                // than as this daemon having handed over on purpose. The ledger
                // itself stays, and must — the daemon taking the store over
                // reads it, and a successor that primed instead would swallow
                // every level standing at the moment of the hand-over.
                attention::stand_down(store);
                // Always said, not only when verbose. A process that ends on
                // purpose owes the log the reason, or the next person to look
                // finds a daemon that simply stopped.
                eprintln!("wsp daemon: pid {other} holds this store now — standing down");
                return 0;
            }
        }

        if let Some(stamp) = changed(started_as, crate::util::exe_stamp()) {
            // The tunnels first, and before the `exec`. An `ssh -L` we leave
            // behind goes on holding the socket it bound, and the daemon that
            // comes up in our place cannot bind it — the machine would read as
            // permanently broken for a reason nothing on screen could explain.
            // Safe if the exec then fails: the next tick starts them again.
            tunnels.shutdown();
            // `reload` returns only when it failed to replace us. An install is
            // a copy, not a rename, so there is a window in which the file on
            // disk is a half-written binary — and unlike a panel, of which
            // there are twenty-two, this process is the only one there is.
            // Staying on the old image is always better than not running.
            let e = reload(verbose);
            eprintln!("wsp daemon: could not reload: {e}");
            // Adopt what we failed on, so a binary that is permanently
            // unexecutable costs one line rather than one every tick, while the
            // next install is still a change and still tried.
            started_as = Some(stamp);
        }

        // Every tick, not only when something woke us: a machine going away is
        // not an event herdr has any way to tell us about, and a tunnel that
        // fell over five minutes ago must not wait for a sidebar hover to be
        // noticed.
        let moved = tunnels.tick(store);
        watch_machines(&tunnels, &readers, &far, verbose);
        if verbose && !moved.is_empty() {
            for name in &moved {
                match store.machine_live(name) {
                    Some(l) if l.reachable => eprintln!("wsp daemon: {name} up"),
                    Some(l) => eprintln!(
                        "wsp daemon: {name} {} — {}",
                        l.tunnel,
                        if l.error.is_empty() { "no answer yet" } else { &l.error }
                    ),
                    None => eprintln!("wsp daemon: {name} gone"),
                }
            }
        }

        // Above the gate below, and that placement is the whole point of this
        // task. A stopped agent changes nothing in the store and raises no
        // event this daemon subscribes to — `pane.agent_status_changed` is
        // per-pane and cannot be asked for globally, see `EVENTS` — so a pass
        // that ran only when something woke us would be a pass that never ran
        // on the one condition it exists to notice. It has its own interval and
        // it is a passenger on this loop, never a reason to sync.
        // One reading of the clock for both, so a tick can never be recorded
        // at a time the due check did not make it due for.
        let now = crate::util::epoch_secs();
        let mut attention_moved = false;
        if pass.due(now) {
            let mut source = attention::machine_source(store);
            let emits = attention::tick(store, &mut pass, &mut source, now);
            // An edge is exactly the moment the sidebar's `needs` token
            // changes, and nothing else below would notice: `store.fingerprint`
            // walks `projects/` and `tasks/`, and a level going up moves
            // neither. Without this the token is correct and up to `REFRESH`
            // late, which on the one signal that is *somebody is waiting on
            // you* is the fault being fixed with a smaller number on it.
            attention_moved = !emits.is_empty();
            if verbose {
                for e in &emits {
                    eprintln!(
                        "wsp daemon: attention {} {} {} — {}",
                        e.edge.word(),
                        e.signal.kind.word(),
                        e.signal.subject,
                        e.signal.detail
                    );
                }
            }
        }

        let fingerprint = store.fingerprint();
        let store_changed = fingerprint != last_fingerprint;
        last_fingerprint = fingerprint;

        let force = last_refresh.elapsed() >= REFRESH;
        if !woken && !store_changed && !force && !attention_moved {
            continue;
        }
        if force {
            last_refresh = Instant::now();
        }

        match sync::sync(store, &mut cache, force) {
            Ok(r) => {
                if verbose && (r.workspaces > 0 || r.panes > 0 || r.reaped > 0) {
                    eprintln!(
                        "wsp daemon: {} workspaces, {} panes, {} bindings reaped{}",
                        r.workspaces,
                        r.panes,
                        r.reaped,
                        if force { " (refresh)" } else { "" }
                    );
                }
            }
            Err(e) => {
                eprintln!("wsp daemon: sync failed: {e}");
                std::thread::sleep(Duration::from_secs(2));
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The husk test's evidence, and the field it is read out of: `ps -E`
    /// prints the command and the environment as one blob, so the pane a
    /// process is in is there to be had without asking herdr — which could not
    /// answer it anyway, since herdr's pane knows its label and not what is
    /// running inside it.
    #[test]
    fn a_panel_is_found_by_the_pane_its_environment_names() {
        let state = Store::default_state();
        let line = |s: &str| vec![(1u32, 1u32, s.to_string())];

        assert_eq!(
            manned(&line("/usr/local/bin/wsp panel HERDR_PANE_ID=w1:p2 SHELL=/bin/zsh"), &state),
            ["w1:p2".to_string()].into_iter().collect()
        );
        // The board, the detail pane and the fullscreen tree are ours too, and
        // are not `wsp panel`. Anything of ours in the pane holds it.
        assert_eq!(
            manned(&line("wsp detail t-1 HERDR_PANE_ID=w1:p3"), &state).len(),
            1
        );
        // Somebody else's binary in a pane of ours says nothing about ours.
        assert!(manned(&line("/bin/zsh -l HERDR_PANE_ID=w1:p6G"), &state).is_empty());
        // A sandbox's panel is not this store's, and its pane is not one this
        // store may close — the same filter that keeps `doctor` from crying
        // wolf about a second daemon.
        assert!(manned(
            &line("wsp panel WSP_STATE=/tmp/sandbox/state HERDR_PANE_ID=w1:p2"),
            &state
        )
        .is_empty());
        // A wsp with no pane in its environment is a wsp somebody typed at a
        // shell outside herdr. It holds nothing.
        assert!(manned(&line("wsp panel"), &state).is_empty());
    }

    /// The guard that would have stopped robustness-021 on its own.
    ///
    /// A `wsp daemon` was started by a sandbox's herdr from the global
    /// `plugins.json`, holding that session's socket and — because nobody told
    /// it otherwise — the live store. Its first sync asks an empty herdr for
    /// panes and reaps every binding in the real store against the answer.
    ///
    /// Both halves of the store have to be named, not just `WSP_HOME`: state is
    /// where bindings and claims live, and a scratch store with live state is
    /// the same accident wearing a different hat (robustness-011).
    #[test]
    fn a_daemon_on_somebody_elses_herdr_must_be_told_which_store() {
        // Built from this machine's home rather than a literal: the rule is
        // "not the default socket", and a test that had to set `HOME` to say so
        // would be a test no other test could run beside.
        let home = crate::util::home();
        let sock = home.join(".config/herdr/sessions/wsp-w1/herdr.sock").display().to_string();
        let default = home.join(".config/herdr/herdr.sock").display().to_string();
        let (sock, default) = (sock.as_str(), default.as_str());

        // Exactly what was found running: a session socket and no store.
        assert!(
            misdirected(Some("wsp-w1"), Some(sock), None, None).is_some(),
            "the daemon that reaps the live store would have started"
        );
        // Half-told is not told: state is where bindings and claims are.
        assert!(misdirected(Some("wsp-w1"), Some(sock), Some("/tmp/s/wsp"), None).is_some());
        assert!(misdirected(Some("wsp-w1"), Some(sock), None, Some("/tmp/s/state")).is_some());
        // A sandbox that says which instance it is, is what this is for.
        assert!(
            misdirected(Some("wsp-w1"), Some(sock), Some("/tmp/s/wsp"), Some("/tmp/s/state"))
                .is_none(),
            "a properly made sandbox was refused its own daemon"
        );

        // The ordinary daemon is never refused, whatever its store — this
        // machine's herdr is the one case where the default store is right.
        assert!(misdirected(None, None, None, None).is_none());
        assert!(misdirected(None, Some(default), None, None).is_none());
        assert!(misdirected(Some(""), Some(""), None, None).is_none());
        // …including a deliberate non-default store on the real socket, which
        // is `WSP_HOME=… wsp daemon` and nobody's accident.
        assert!(misdirected(None, Some(default), Some("/tmp/s/wsp"), None).is_none());

        // A socket somewhere else with no session name is the same danger: the
        // socket is what decides, and it is the half wsp can observe for itself.
        assert!(misdirected(None, Some("/tmp/somewhere/herdr.sock"), None, None).is_some());

        // …and a session name on the *default* socket is not, however it is
        // spelled. herdr calls the main session `default`, so a server that one
        // day exported `HERDR_SESSION` to its startup plugins would otherwise
        // have this refuse the real daemon on every restart — a naming
        // convention we do not own deciding whether the machine syncs.
        for named in ["default", "wsp-w1", "anything at all"] {
            assert!(
                misdirected(Some(named), Some(default), None, None).is_none(),
                "HERDR_SESSION={named} on this machine's own socket refused the ordinary daemon"
            );
            assert!(misdirected(Some(named), None, None, None).is_none());
        }
        // The name is still what the message leads with, because it is better
        // diagnostics than a path.
        assert!(
            misdirected(Some("wsp-w1"), Some(sock), None, None)
                .is_some_and(|m| m.contains("herdr session `wsp-w1`")),
            "the refusal stopped saying which session it was about"
        );
    }

    /// The marker a crash leaves behind must never be the reason a machine has
    /// no daemon. This is the case t-260817-052 says will happen most: herdr
    /// died, its daemon did not, and the pid in the marker is not there any more
    /// — or, on the 15th's numbers coming round again, is something else
    /// entirely wearing that pid.
    #[test]
    fn a_marker_left_by_a_dead_daemon_does_not_stop_the_next_one() {
        // Nothing running, marker from Saturday.
        assert_eq!(claim(Some(22121), 88994, Some(&[])), Claim::Take(Some(22121)));
        // Alive, but somebody else's process on a reused number.
        assert_eq!(claim(Some(22121), 88994, Some(&[70001])), Claim::Take(Some(22121)));
        // No marker at all: the first daemon on a fresh store.
        assert_eq!(claim(None, 88994, Some(&[])), Claim::Take(None));

        // And the case we cannot corroborate. `ps` not answering leaves the
        // marker worth no more than an empty one — refusing on it would trade
        // two daemons for none, which is the worse of the two failures because
        // nothing on screen would say so.
        assert_eq!(
            claim(Some(22121), 88994, None),
            Claim::Take(Some(22121)),
            "refused on a marker nothing could confirm, leaving the store with no daemon"
        );
    }

    /// The one case that is a real contest, and the house answer to it. The
    /// incumbent is running and answering, so the newcomer is not more correct
    /// than it: a newcomer on the wrong herdr was refused earlier by
    /// `misdirected`, and an incumbent on an old binary replaces itself.
    #[test]
    fn a_second_daemon_on_a_store_a_live_one_holds_refuses_and_says_which_pid() {
        assert_eq!(claim(Some(22121), 88994, Some(&[22121])), Claim::Refuse(22121));
        assert_eq!(claim(Some(22121), 88994, Some(&[70001, 22121])), Claim::Refuse(22121));
    }

    /// `reload` execs, so the pid survives it and the new image reads a marker
    /// holding its own number. Refusing there would end the machine's daemon on
    /// the next `wsp install` — every pane re-execs and the daemon quietly does
    /// not come back — which is a worse bug than the one the marker is for.
    #[test]
    fn a_daemon_that_reloaded_into_a_new_image_does_not_refuse_itself() {
        assert_eq!(claim(Some(88994), 88994, Some(&[])), Claim::Keep);
        assert_eq!(claim(Some(88994), 88994, Some(&[88994])), Claim::Keep);
        assert_eq!(standing(Some(88994), 88994), Standing::Ours);
    }

    /// Two daemons starting in the same instant both read an empty marker and
    /// both write it, so the start-time check cannot be the only one. Whoever is
    /// not in the marker afterwards ends, which is the same guarantee arrived at
    /// from the other side — and a marker that has gone entirely is claimed
    /// again rather than left for `doctor` to report as a stray for ever.
    #[test]
    fn the_daemon_the_marker_does_not_name_stands_down_at_its_next_tick() {
        assert_eq!(standing(Some(22121), 88994), Standing::Lost(22121));
        assert_eq!(standing(None, 88994), Standing::Unmarked);
    }

    /// What counts as a daemon in a `ps -E` line, where the environment is
    /// appended to the command in one field. Matching anywhere in the line would
    /// count every agent's shell, because an agent's environment mentions wsp
    /// several times over.
    #[test]
    fn only_a_process_whose_command_is_wsp_daemon_is_a_daemon() {
        assert!(is_daemon("/Users/e/.local/bin/wsp daemon PATH=/usr/bin HOME=/Users/e"));
        assert!(is_daemon("wsp daemon"));
        // Flags are somebody's business and not ours: `-v` is how the daemon
        // found on the 15th had been started.
        assert!(is_daemon("/Users/e/.local/bin/wsp daemon -v HERDR_SESSION=default"));

        assert!(!is_daemon("/Users/e/.local/bin/wsp panel PATH=/usr/bin"));
        assert!(!is_daemon("wsp"));
        assert!(!is_daemon(""));
        // A shell that ran a wsp command, and whose environment says so. `_` is
        // set by every interactive shell to the last command it ran.
        assert!(!is_daemon("-zsh _=/Users/e/.local/bin/wsp WSP_DAEMON=1"));
        assert!(!is_daemon("tail -f daemon.log PATH=/usr/bin"));
        // Something else called wsp: the name has to be the whole leaf.
        assert!(!is_daemon("/usr/bin/notwsp daemon"));
    }

    /// A sandbox runs a daemon of its own, deliberately, with `WSP_STATE` set —
    /// so a check that counted it would cry wolf on a machine where sandboxes
    /// are how a change gets tested (robustness-021 is why they exist). The store
    /// is read out of the environment exactly as `Store::open` reads it: told, or
    /// the default.
    #[test]
    fn a_daemon_counts_only_against_the_store_it_is_pointed_at() {
        let live = Store::default_state();
        let sandbox = std::path::PathBuf::from("/tmp/wsp-sb/state");

        let plain = "wsp daemon PATH=/usr/bin";
        let told = format!("wsp daemon WSP_HOME=/tmp/wsp-sb/wsp WSP_STATE={}", sandbox.display());

        assert!(for_store(plain, &live), "the ordinary daemon was not counted against the live store");
        assert!(!for_store(plain, &sandbox));
        assert!(for_store(&told, &sandbox));
        assert!(!for_store(&told, &live), "a sandbox's daemon was counted against the live store");

        // A key that ends in ours is not ours.
        assert!(for_store("wsp daemon OLD_WSP_STATE=/tmp/elsewhere", &live));
    }

    /// What `doctor` says, and the two things it must not say: that a machine
    /// whose `ps` would not answer has no daemon, and that a machine with one
    /// daemon has a problem.
    #[test]
    fn doctor_calls_two_daemons_a_problem_and_one_a_note() {
        let check = |holder: Option<u32>, running: Option<&[u32]>, herdr_up: bool| {
            let (mut p, mut n) = (Vec::new(), Vec::new());
            health(holder, running, herdr_up, &mut p, &mut n);
            (p, n)
        };

        // The forty-two hours, as they would have read on the 17th.
        let (problems, _) = check(Some(88994), Some(&[22121, 88994]), true);
        assert!(!problems.is_empty(), "two daemons passed the integrity check");
        assert!(
            problems[0].contains("22121") && problems[0].contains("88994"),
            "the problem did not say which pids: {problems:?}"
        );
        assert!(
            problems.iter().any(|p| p.contains("kill 22121") && !p.contains("kill 88994")),
            "doctor did not name the stray, or offered to kill the one holding the store: {problems:?}"
        );

        // The same two before either of them could write a marker, which is
        // every machine until it next restarts.
        let (problems, _) = check(None, Some(&[22121, 88994]), true);
        assert_eq!(problems.len(), 2, "two daemons went unreported for want of a marker");

        // One is the whole point, and is not a problem however it is marked.
        for holder in [Some(88994), None, Some(22121)] {
            let (problems, notes) = check(holder, Some(&[88994]), true);
            assert!(problems.is_empty(), "one daemon reported as a fault: {problems:?}");
            assert_eq!(notes.len(), 1, "one daemon said more than one line about itself");
        }

        // None, and whether that is worth saying. A machine with no herdr on it
        // is a machine wsp works on and does not want a daemon for.
        assert_eq!(check(None, Some(&[]), true).1.len(), 1, "a herdr with no daemon went unsaid");
        assert!(check(None, Some(&[]), false).1.is_empty(), "a machine with no herdr was told to run a daemon");

        // And the answer we do not have. `ps` refusing is not an empty machine.
        let (problems, notes) = check(Some(88994), None, true);
        assert!(
            problems.is_empty() && notes.is_empty(),
            "doctor pronounced on a process list it could not read: {problems:?} {notes:?}"
        );
    }

    /// The reload gate, and the two readings that must not open it. A daemon
    /// that could not read its own path at startup has nothing to compare
    /// against, and one that cannot read it *now* is most likely looking at a
    /// file mid-install — the moment when exec'ing it is worst. Both have to
    /// answer "no change", because the caller's next move on any answer but
    /// `None` is to replace this process with what it found.
    #[test]
    fn only_a_stamp_we_can_read_and_did_not_start_with_is_a_reload() {
        let started = Some((100, 1_800_000_000));

        assert_eq!(changed(started, None), None, "an unreadable binary read as a new one");
        assert_eq!(changed(None, Some((100, 1))), None, "reloaded with nothing to compare against");
        assert_eq!(changed(started, started), None, "the binary we started with read as new");
        assert_eq!(changed(None, None), None, "reloaded knowing nothing at all");

        assert_eq!(
            changed(started, Some((100, 1_800_000_001))),
            Some((100, 1_800_000_001)),
            "a same-length reinstall was not noticed"
        );
        assert_eq!(
            changed(started, Some((101, 1_800_000_000))),
            Some((101, 1_800_000_000)),
            "a reinstall inside the same second was not noticed"
        );
    }
}
