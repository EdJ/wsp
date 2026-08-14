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

const DEBOUNCE: Duration = Duration::from_millis(400);
const TICK: Duration = Duration::from_secs(20);
const REFRESH: Duration = Duration::from_secs(15 * 60);

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
    "pane.agent_status_changed",
    "worktree.created",
    "worktree.opened",
];

pub fn run(store: &Store, verbose: bool) -> i32 {
    if !herdr::available() {
        eprintln!("wsp: no herdr socket at {}", herdr::socket_path().display());
        return 1;
    }

    let (tx, rx) = mpsc::channel::<()>();

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

    let mut cache = Cache::default();
    let mut last_refresh = Instant::now();
    let mut last_fingerprint = store.fingerprint();

    // Before the first sync: herdr has just come back with its workspaces and
    // agent sessions restored, but pane ids are new and no binding survived.
    // Claims did, so rebuild from those.
    let rebound = crate::cmd_agent::reconcile(store);
    let dropped = store.reap_claims(
        &store.tasks().into_iter().map(|t| t.id).collect::<Vec<_>>(),
    );
    if verbose && (rebound > 0 || dropped > 0) {
        eprintln!("wsp daemon: reconciled {rebound} binding(s), dropped {dropped} stale claim(s)");
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
