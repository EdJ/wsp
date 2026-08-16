//! The only long-lived piece: herdr events + store polling -> token refresh.
//!
//! Deliberately dependency-free. One thread blocks on the event stream and
//! feeds a channel; the main loop coalesces bursts (herdr emits
//! `workspace_focused` on every sidebar hover) and refreshes TTLs on a timer.

use std::sync::mpsc::{self, RecvTimeoutError};
use std::time::{Duration, Instant};

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

pub fn run(store: &Store, verbose: bool) -> i32 {
    if !herdr::available() {
        eprintln!("wsp: no herdr socket at {}", herdr::socket_path().display());
        return 1;
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

        // Here, at the top, because it is the one point in the loop where
        // nothing is half-done: no sync in flight, no store lock held, the
        // debounce already drained. Everything below this line either finishes
        // or is not started.
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

        let fingerprint = store.fingerprint();
        let store_changed = fingerprint != last_fingerprint;
        last_fingerprint = fingerprint;

        let force = last_refresh.elapsed() >= REFRESH;
        if !woken && !store_changed && !force {
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
