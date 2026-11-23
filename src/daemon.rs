//! The only long-lived piece: a timer that drives the run, the repairs, the
//! attention pass and the far machines' tunnels.
//!
//! "The only" is now enforced rather than assumed — one daemon per store, and
//! the argument for how a second one is turned away is under *one daemon per
//! store* below.
//!
//! **It speaks to compound and nothing else** (`wsp-544`, `wsp-120`). Nothing
//! wakes it early: every pass below runs on its own interval, off a [`TICK`].
//!
//! It carries one passenger, and being the only long-lived process is the whole
//! qualification for it: [`crate::attention`] derives what is wrong on this
//! machine once a minute and puts the changes where a hook can see them. That
//! is the one thing in wsp that happens at nobody's request — every other
//! surface states the same facts to whoever is looking.

use std::path::{Path, PathBuf};
use std::time::Duration;

use crate::attention;
use crate::store::Store;
use crate::tunnel::Supervisor;

const TICK: Duration = Duration::from_secs(20);

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
/// process id, and so launchd's idea of what it started, survives. Nothing is
/// owed to the passes either: this is called at the top of the loop, where the
/// last one has finished and the next has not begun.
fn reload(verbose: bool) -> std::io::Error {
    let exe = match wsp_base::util::self_exe() {
        Ok(e) => e,
        Err(e) => return e,
    };
    let mut c = wsp_base::calls::Command::new(exe);
    c.arg("daemon");
    // Carried across deliberately: a daemon that went quiet the first time it
    // reloaded would look exactly like one that had died.
    if verbose {
        c.arg("-v");
    }
    c.exec()
}


/// Wait on the children a reload carried over, and say which they were.
///
/// The daemon's 155 `<defunct>` children (wsp-667) were most likely the detached `wsp`
/// processes its tick launches and hands to a reaping thread: `govern <scope>
/// --reseat` (`repair`), `worklist advance --barrier-run` and the `--end-check`
/// waits (`cycle::launch`), and the `--ending`/despawn helpers `when_idle`
/// starts. Each lives for minutes. An install `exec`s the daemon, which keeps
/// the children and ends the threads, so one alive at a reload went defunct
/// when it ended. An agent's `compound-sup` is never among them: it is the
/// child of whichever of those processes started it, and goes to pid 1 when
/// that one exits. A daemon started fresh has none, and says nothing.
fn adopt_carried_over(store: &Store) {
    let kids = wsp_base::calls::children_of(crate::host::LOCAL.own_children());
    if kids.is_empty() {
        return;
    }
    wsp_base::calls::adopt(&kids);
    let named: Vec<String> = kids.iter().map(|(pid, line)| format!("{pid} {line}")).collect();
    crate::store::log_line(store, &format!("daemon: waiting on {} child(ren) from before the reload: {}", kids.len(), named.join("; ")));
}

/// Is the per-tick reaper turned off (`WSP_NO_REAP` set, not empty, not `0`)?
fn reaper_off() -> bool {
    std::env::var_os("WSP_NO_REAP").is_some_and(|v| !v.is_empty() && v != "0")
}

/// Is there nothing at all for the daemon to run for?
///
/// `place::local_backends()` is the fold every other reader — `wip`, `tell`,
/// `census` — uses, and `Compound` surveys a local directory and never fails
/// to answer it. So this is `true` only when literally nothing does — every
/// backend's [`crate::place::Place::census`] came back `Err` — which in
/// practice means the process list or the compound state directory itself
/// could not be read.
fn nothing_answered(backend_ok: &[bool]) -> bool {
    backend_ok.iter().all(|ok| !ok)
}

// ---- one daemon per store ---------------------------------------------------
//
// Two of these ran for forty-two hours and nothing noticed (t-260817-052). One
// was started by hand on the 15th with `-v`, outlived the session that started
// it, and was still there when a startup hook started a second. So the trigger
// is not carelessness: it is any crash or restart, and the hand-started daemon
// that outlives the terminal it was typed in is the ordinary way to get one.
//
// Both then do everything twice against one store: refresh TTLs, run the
// cycle, and repair. render-040 is the record of what one wrong reap costs.
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
/// is not the daemon.
fn is_wsp(rest: &str, sub: &str) -> bool {
    let mut words = argv(rest);
    // `-v` and anything after it are somebody's flags, not our business.
    is_exe(words.next()) && words.next() == Some(sub)
}

/// The words of that field that are argv: everything before the first
/// `KEY=value`, which is where the command stops and `ps -E`'s environment
/// begins.
pub(crate) fn argv(rest: &str) -> impl Iterator<Item = &str> {
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
    state_of(rest) == state
}

/// The state directory a `ps -E` line's environment points at.
pub(crate) fn state_of(rest: &str) -> PathBuf {
    match rest.split_whitespace().find_map(|w| w.strip_prefix("WSP_STATE=")) {
        Some(v) => PathBuf::from(v),
        None => Store::default_state(),
    }
}

/// Every `wsp daemon` on this machine pointed at `state`, or `None` when the
/// process list could not be read at all — which no caller may read as "none",
/// because a machine with no processes on it is a machine that cannot answer.
pub(crate) fn running(state: &Path) -> Option<Vec<u32>> {
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
fn daemons(all: &[(u32, u32, String)], me: u32, state: &Path) -> Vec<u32> {
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
pub(crate) fn a_backend_answered() -> bool {
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
pub(crate) fn loud(running: Option<&[u32]>, backend_up: bool) -> Option<String> {
    match running {
        Some([]) if backend_up => Some(
            "no wsp daemon — tokens, TTLs and the unattended pass are all off, and nothing will wake a seat (`wsp install` loads one)"
                .into(),
        ),
        _ => None,
    }
}

/// The tailnet addresses `tailscale ip` names. The manager's parse
/// (`wsp_manager::tailnet_addrs`), copied: the root links neither the manager
/// nor the host crate outside tests.
fn tailnet_addrs() -> Vec<std::net::IpAddr> {
    let out = std::process::Command::new("tailscale").arg("ip").output().ok().filter(|o| o.status.success());
    let Some(out) = out else { return Vec::new() };
    String::from_utf8_lossy(&out.stdout)
        .lines()
        .filter_map(|l| l.trim().parse::<std::net::IpAddr>().ok())
        .filter(|ip| wsp_server::allowed(ip) && !ip.is_loopback())
        .collect()
}

/// What `start_api` started; ended with the daemon.
pub(crate) struct Api {
    pub(crate) db: wsp_server::SharedStore,
    _server: Option<wsp_server::Server>,
    _mirror: Option<(std::thread::JoinHandle<()>, std::sync::Arc<std::sync::atomic::AtomicBool>)>,
    /// wsp-743: the census reads' inputs, beside the mirror's entities.
    _readings: Option<(std::thread::JoinHandle<()>, std::sync::Arc<std::sync::atomic::AtomicBool>)>,
}

/// Open `state.db`, serve it, and start the mirror thread. Each failure is one
/// `cycle.log` line and the rest goes on.
pub(crate) fn start_api(store: &Store) -> Option<Api> {
    if crate::mirror::api_off() {
        return None;
    }
    let say = |line: String| wsp_store::store::log_line(store, &line);
    let db: wsp_server::SharedStore =
        match wsp_store_sqlite::SqliteStore::open(store.state.join("state.db"), std::sync::Arc::new(wsp_server::Mirror)) {
            Ok(db) => std::sync::Arc::new(db),
            Err(e) => {
                say(format!("api: state.db did not open ({e:?}); the daemon runs without the API"));
                return None;
            }
        };
    let mut cfg = wsp_server::Config::new(&store.state);
    cfg.port = crate::mirror::api_port();
    cfg.addrs = tailnet_addrs();
    let server = match wsp_server::Server::start(db.clone(), cfg) {
        Ok(s) => Some(s),
        Err(e) => {
            say(format!("api: the server did not start ({e}); the mirror runs on"));
            None
        }
    };
    let mirror = Some(crate::mirror::start(store, db.clone()));
    let readings = crate::readings::start(store, db.clone());
    Some(Api { db, _server: server, _mirror: mirror, _readings: readings })
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

static TERMED: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

extern "C" fn on_term(_: i32) {
    TERMED.store(true, std::sync::atomic::Ordering::SeqCst);
}

extern "C" {
    fn signal(sig: i32, handler: usize) -> usize;
}

/// Catch SIGTERM and SIGINT; a watcher thread reaps the tunnel masters and
/// exits 0 (`KeepAlive` is `SuccessfulExit=false`, so launchd does not restart
/// a daemon that was asked to go). Not under test: a test process must keep
/// its own signal handling.
fn reap_on_term(store: &Store) {
    if cfg!(test) {
        return;
    }
    unsafe {
        signal(15, on_term as extern "C" fn(i32) as usize);
        signal(2, on_term as extern "C" fn(i32) as usize);
    }
    let state = store.state.clone();
    std::thread::spawn(move || loop {
        std::thread::sleep(Duration::from_millis(200));
        if TERMED.load(std::sync::atomic::Ordering::SeqCst) {
            crate::tunnel::reap_masters(&state);
            wsp_base::calls::exit(0);
        }
    });
}

/// How long `cycle.log` may be quiet before a machine with no daemon process
/// is told its fleet has stopped (wsp-449).
const SILENT_SECS: u64 = 60;

/// [`loud`], plus the two things a dropped launchd job leaves: the note an
/// install that could not load it keeps, and a `cycle.log` quiet for over a
/// minute. Both only when no daemon process is running — a log can be quiet
/// for good reasons while one is — so a live daemon is never called dead.
pub(crate) fn dead(state: &Path, running: Option<&[u32]>, backend_up: bool) -> Option<String> {
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

pub fn run(store: &Store, verbose: bool) -> i32 {
    // Its own record is one line per tick (`wsp-481`), not one for the process.
    wsp_base::calls::quiet();
    // Before anything is spawned: a child here already is one the image before
    // an `exec`-reload started, whose reaping thread the exec ended (wsp-667).
    adopt_carried_over(store);
    let backend_ok: Vec<bool> =
        crate::place::local_backends().iter().map(|b| b.census().is_ok()).collect();
    if nothing_answered(&backend_ok) {
        eprintln!("wsp: no backend answered — compound has nothing to run for");
        return 1;
    }

    // Second gate, and after the first on purpose: a daemon that is going to
    // refuse for a sharper reason should never have written itself into the
    // marker, and one with nothing at all to run for is not the daemon this
    // store wants to be holding it.
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
                "wsp: two of them run the cycle and the repairs twice against one store — `kill {pid}` first if this one is meant to replace it"
            );
            // Not a failure. What the caller wanted — a daemon on this store —
            // is what is running, and whatever started this one gets a
            // process that did its job rather than one that looks broken.
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

    // `launchctl bootout` ends this process with SIGTERM, which skips every
    // `tunnels.shutdown()` below and leaves the ssh ControlMasters behind;
    // one orphaned master blocked a machine until closed by hand (wsp-444).
    // So the signal is caught, and the masters are asked to exit before we do.
    reap_on_term(store);

    // Under `WSP_HOST=manager`, the manager's events on a thread of their own,
    // not paced by the tick (`docs/agent-manager.md` 3.4; wsp-028): each is
    // folded into the store's files as it comes. Under `inproc`, nothing.

    // The state core's database, its API and the mirror of the files into it
    // (wsp-075). Beside everything else the daemon does; `WSP_API=off` starts
    // none of it, and nothing here ends the run.
    let _api = start_api(store);
    let _fold = crate::fold::start(store, _api.as_ref().map(|a| a.db.clone()));

    // Every executor's ssh master, held open on this machine. Inert until
    // somebody runs `wsp machine add`; see `crate::tunnel`.
    let mut tunnels = Supervisor::new();
    // The half of this process that looks. Its ledger lives
    // in the store and not in this variable, so the `exec` below costs it
    // nothing; see `attention::load`.
    let mut pass = attention::Pass::new();
    // The reconciler's own clock, and separate from the attention pass's for the
    // reason the daemon's own comment above the loop gives: this one exists for
    // conditions that raise no event, and sharing a gate with a pass that *does*
    // would tie "somebody woke us" to "a member's agent exited".
    let mut repair_pass = crate::repair::Pass::new();
    // When seats' resident memory was last sampled (`wsp-066`).
    let mut memory_at = 0i64;
    // What we are executing, so we can notice an `install` landing underneath
    // us. See `reload` at the top of the loop.
    let mut started_as = crate::util::exe_stamp();

    // Claims and worked records for tasks that no longer exist. Store-only
    // (`wsp-544`).
    let live: Vec<String> = store.tasks().into_iter().map(|t| t.id).collect();
    let dropped = store.reap_claims(&live) + store.reap_worked(&live);
    if verbose && dropped > 0 {
        eprintln!("wsp daemon: dropped {dropped} stale claim(s)");
    }

    // The first pass runs at once; every later one a `TICK` after the last.
    let mut first = true;
    loop {
        if !std::mem::take(&mut first) {
            // The wait between ticks, in slices while a far machine is not
            // answering: each slice looks at those machines again, so a link
            // that comes back is redialled within seconds and not on the next
            // tick after a backoff (wsp-561). With every machine up — or none —
            // this is the one sleep it always was.
            let until = std::time::Instant::now() + TICK;
            loop {
                let left = until.saturating_duration_since(std::time::Instant::now());
                if left.is_zero() {
                    break;
                }
                if !tunnels.recovering() {
                    std::thread::sleep(left);
                    break;
                }
                std::thread::sleep(left.min(crate::tunnel::RECOVER_EVERY));
                if std::time::Instant::now() < until {
                    for name in tunnels.recover(store) {
                        if verbose {
                            eprintln!("wsp daemon: {name} {}", store.machine_live(&name).map(|l| l.tunnel).unwrap_or_default());
                        }
                    }
                }
            }
        }

        // `wsp-481`: one line in `calls.jsonl` per pass of this loop, with ms
        // per phase, so a slow phase shows up by name. The sleep above is not
        // work, and is left out of it.
        wsp_base::calls::tick_begin();
        crate::place_compound::tick_deliver_budget(true);
        // The liveness stamp, at the top of the pass so a slow pass still
        // reads as ticking (wsp-035).
        store.stamp_daemon_tick(crate::util::epoch_secs());
        wsp_base::calls::phase("standing");
        // Whatever this process spawned and nothing waited for is a `<defunct>`
        // entry until the daemon exits, which is days (wsp-664).
        // Only children it knows nothing waits on are taken (wsp-667); one it
        // knows nothing of is named, so the spawn that leaks it can be found.
        // `WSP_NO_REAP=1` turns it off, for the run that shows nothing is left
        // to it: an hour with no reaper and no `<defunct>` child (wsp-667 d1).
        let reap = match reaper_off() {
            true => Default::default(),
            false => wsp_base::calls::reap_defunct_with(|| crate::host::LOCAL.own_defunct()),
        };
        for (pid, line) in &reap.reaped {
            crate::store::log_line(store, &format!("daemon: reaped defunct child {pid}, nothing was waiting on it: {line}"));
        }
        for (pid, name) in &reap.strays {
            crate::store::log_line(store, &format!("daemon: child {pid} ({name}) is defunct and nothing in wsp holds it — a spawn that is never waited on; left for its owner"));
        }

        // Both of the next two checks are here, at the top, because it is the
        // one point in the loop where nothing is half-done: no pass in flight,
        // no store lock held. Everything below
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

        // Every tick: a tunnel that fell over five minutes ago is noticed on
        // the next one. Far seats are reached through compound-sup's per-seat
        // forwards on these masters, which `tunnels.tick` puts back.
        wsp_base::calls::phase("tunnels");
        let moved = tunnels.tick(store);
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

        // The tells held for a far seat go the tick its link answers and the
        // seat is ready, not at the attention pass's own interval, which is
        // longer and can fall wholly outside a short ready window (wsp-639).
        // Cheap when nothing is held.
        crate::wake::flush_held_tells_now(store);

        // On its own interval, every tick: a stopped agent changes nothing in
        // the store and raises no event, so a pass that ran only when something
        // woke us would never run on the one condition it exists to notice.
        // One reading of the clock for both, so a tick can never be recorded
        // at a time the due check did not make it due for.
        let now = crate::util::epoch_secs();
        wsp_base::calls::phase("attention");
        if pass.due(now) {
            // One walk of the store for the whole pass (`wsp-320`): it reads
            // the tasks once per seat, and a `stat` of every task per reading
            // was most of what an idle pass cost. See `Store::steady`.
            let emits = store.steady(|| {
                let mut source = attention::machine_source(store);
                attention::tick(store, &mut pass, &mut source, now)
            });
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

        // Seats gone with nothing to say leave the state core and the files
        // that name them, every tick (wsp-094): what a machine restart left
        // clears on the first tick after it.
        if let Some(api) = &_api {
            wsp_base::calls::phase("sweep:mute-seats");
            store.steady(|| crate::fold::sweep_mute(store, &api.db, |seats| crate::cmd_agent::mute_seats(store, seats)));
        }

        // **The run's own steps, and the repairs, on the same timer** — `wsp-147`.
        //
        // On a timer for the reason the attention pass is: a member whose
        // agent exited raises no event and writes no record. It runs in this process rather than as a `wsp worklist advance` per list
        // because that would be a process per running list every minute, and
        // because `cycle::step` is idempotent: a tick racing a verb's own
        // advance finds the records and starts nothing twice. And since
        // `wsp-663` they do not race at all: a step holds its list, and the
        // second one marks it to step again (`docs/step-lock.md`).
        //
        // Silent about its own failures. `repair::tick` writes what it did to
        // `cycle.log`, which is where the run's history is read from, and a
        // daemon stderr line nobody has attached to would be a second place to
        // look for the same answer.
        if repair_pass.due(now) {
            repairs(store, &crate::cycle::Fleet, &mut repair_pass);
        }

        // Every seated agent's resident memory, on its own interval, so a
        // retro can read what a seat held over its seated time after the seat
        // is gone (`wsp-066`).
        if now - memory_at >= crate::memory::SAMPLE_EVERY {
            memory_at = now;
            wsp_base::calls::phase("seat-memory");
            sample_seat_memory(store, now);
        }

        // The CPU burners standing in no tree, for `wsp wip`'s load warning
        // (wsp-691), read here so `wip` itself runs no `ps` (wsp-716).
        wsp_base::calls::phase("burners");
        crate::cmd_role::stamp_burners(store, now);

        crate::place_compound::tick_deliver_budget(false);
        wsp_base::calls::tick_end();
    }
}

/// One sample of every seated agent's resident memory into
/// `seat-memory.json`, off the manager's `seats` replies.
fn sample_seat_memory(store: &Store, now: i64) {
    let reading = crate::host_tcp::census(&store.state, env!("CARGO_PKG_VERSION"));
    let rss: std::collections::BTreeMap<String, u64> = reading.rows.iter().map(|(s, r)| (s.clone(), r.rss_bytes)).collect();
    crate::memory::sample_rss(&store.state, &rss, now);
}

/// The repair pass, and the sweeps after it, as one tick runs them.
///
/// The manager's `seat.stopped` is the cause `sweep:ended-panes` looked for, and
/// it is folded ([`crate::fold`]) (wsp-028, `docs/agent-manager.md` 4.3): that
/// sweep is gone.
fn repairs(store: &Store, seats: &dyn crate::cycle::Seats, pass: &mut crate::repair::Pass) {
    store.steady(|| crate::repair::tick(store, seats, pass));
    // Answered retro questions plan their list (`wsp-472`); idempotent.
    wsp_base::calls::phase("retro");
    store.steady(|| crate::retro_series::sweep(store));
}

/// The verbs this module answers to, each with the help it carries. `main` and
/// the help text are assembled from these; nothing in `main.rs` names them.
pub const VERBS: &[crate::verbs::Verb] = &[
    crate::verbs::Verb {
        names: &["daemon"],
        run: |store, args| match args.rest.first().map(String::as_str) {
            Some("reload") => crate::cmd_role::daemon_reload(store, running(&store.state)),
            _ => run(store, args.has("verbose")),
        },
        help: &[
            crate::verbs::Help {
                section: crate::verbs::Section::Plumbing,
                order: 80,
                text: r##"  wsp daemon [-v]                   cycle, repair and attention loop (launchd)"##,
            },
            crate::verbs::Help {
                section: crate::verbs::Section::Plumbing,
                order: 81,
                text: r##"  wsp daemon reload                 put the launchd job back when no daemon
                                    answers; the primary seat's, refused if one does"##,
            },
        ],
    },
];

#[cfg(test)]
mod tests {
    /// wsp-449: a machine with no daemon process and a `cycle.log` quiet for
    /// over a minute is told so; a live daemon is never called dead.
    #[test]
    fn a_silent_cycle_log_with_no_daemon_process_is_called_dead() {
        let dir = std::env::temp_dir().join(format!("wsp-449-dead-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        assert_eq!(dead(&dir, Some(&[]), false), None, "no log, nothing to say");
        let log = dir.join("cycle.log");
        std::fs::write(&log, "x\n").unwrap();
        assert_eq!(dead(&dir, Some(&[]), false), None, "a fresh log is a quiet minute");
        let old = std::time::SystemTime::now() - Duration::from_secs(300);
        std::fs::File::options().write(true).open(&log).unwrap().set_modified(old).unwrap();
        let said = dead(&dir, Some(&[]), false).expect("a log silent for 5 minutes");
        assert!(said.contains("not running") && said.contains("cycle.log"), "{said}");
        assert_eq!(dead(&dir, Some(&[4242]), false), None, "a live daemon is not dead");
        let _ = std::fs::remove_dir_all(&dir);
    }

    use super::*;

    /// One backend answering —
    /// compound's local directory, which never fails to survey — is enough to
    /// run for; only every backend failing is "nothing to run for".
    #[test]
    fn the_daemon_runs_if_any_local_backend_answers() {
        assert!(!nothing_answered(&[true, false]), "one down, one up, refused anyway");
        assert!(!nothing_answered(&[false, true]), "order should not matter");
        assert!(!nothing_answered(&[true, true]));
        assert!(nothing_answered(&[false, false]), "nothing answering was let through");
        assert!(nothing_answered(&[]), "an empty backend list answered for nobody");
    }

    /// The marker a crash leaves behind must never be the reason a machine has
    /// no daemon. This is the case t-260817-052 says will happen most: the
    /// session died, its daemon did not, and the pid in the marker is not there any more
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

    /// `wsp-145`, and the whole of what the one sentence is for: a machine whose
    /// backend is up and which has no daemon, said out loud.
    ///
    /// Both exclusions are load-bearing rather than politeness. A machine with
    /// nothing to spawn onto does not need a daemon, and telling
    /// it one is missing is a line in the most expensive output wsp has, on a
    /// machine where it is false. And a `ps` that would not answer is not an
    /// empty machine — `health` has always refused to read it that way, because
    /// the sentence it produces sends somebody to start a *second* daemon.
    #[test]
    fn a_missing_daemon_is_only_ever_said_where_there_is_something_to_miss_it() {
        assert!(loud(Some(&[]), true).is_some(), "the machine that started this row");
        assert!(loud(Some(&[]), false).is_none(), "a machine with no backend needs no daemon");
        assert!(loud(Some(&[42]), true).is_none(), "there is a daemon");
        assert!(loud(None, true).is_none(), "a ps that would not answer is not an empty machine");
    }

    /// The one case that is a real contest, and the house answer to it. The
    /// incumbent is running and answering, so the newcomer is not more correct
    /// than it: an incumbent on an old binary replaces itself.
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
        assert!(is_daemon("/Users/e/.local/bin/wsp daemon -v WSP_HOME=/tmp/s"));

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

    /// A spawn the tick makes runs beneath the daemon, and its brief asks
    /// whether a daemon is running. Excluding ancestors answered "no" there,
    /// on every agent a run started; only our own pid is ours to skip.
    #[test]
    fn an_agent_the_daemon_spawned_still_finds_the_daemon() {
        let live = Store::default_state();
        let all = vec![
            (1, 0, "launchd".to_string()),
            (500, 1, "/Users/e/.local/bin/wsp daemon PATH=/usr/bin".to_string()),
            (600, 500, "/bin/zsh -c claude HOME=/Users/e".to_string()),
            (700, 600, "/Users/e/.local/bin/wsp brief --session _=/Users/e/.local/bin/wsp".to_string()),
        ];
        assert_eq!(daemons(&all, 700, &live), vec![500], "the daemon above us was not counted");
        // The daemon asking at its own claim does not find itself.
        assert_eq!(daemons(&all, 500, &live), Vec::<u32>::new());
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
        let check = |holder: Option<u32>, running: Option<&[u32]>, backend_up: bool| {
            let (mut p, mut n) = (Vec::new(), Vec::new());
            health(holder, running, backend_up, &mut p, &mut n);
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

        // None, and whether that is worth saying. A machine with no backend on
        // it is a machine wsp works on and does not want a daemon for.
        assert_eq!(check(None, Some(&[]), true).1.len(), 1, "a backend with no daemon went unsaid");
        assert!(check(None, Some(&[]), false).1.is_empty(), "a machine with no backend was told to run a daemon");

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

    /// The tick under `WSP_HOST=manager` and under `inproc` (wsp-028, leg 2).
    mod tick_by_host {
        use super::super::repairs;
        use crate::fold;
        use crate::place::State;
        use crate::store::Store;
        use crate::util;
        use serde_json::Value;
        use std::sync::atomic::Ordering;
        use std::time::{Duration, Instant};
        use wsp_host_api::{
            At, Beat, DiskFree, Epoch, Frame, HostFacts, Key, MemoryPressure, Message, OrderFail, OrderId, OrderRefused, RowId,
            SeatDid, SeatId, SeatStarted, SeatStopped, SeatStuck, Thermal, Turn, TurnEdge, FileRef, HostId,
        };
        use wsp_manager::{Config, Manager, TOKEN_FILE};

        const HOST: &str = "tick-host";
        /// The probes a tick under the manager runs none of.
        const PROBES: &[&str] = &["ps", "df", "compound-render", "lsof", "sysctl", "vm_stat", "memory_pressure", "pmset", "ioreg"];
        const WAIT: &str = "until ! pgrep -f 'cargo test'; do sleep 5; done";

        /// `WSP_HOST` for one test, put back however it ends. Taken after
        /// [`util::isolated`], whose lock it is held under.
        struct HostVar;
        impl HostVar {
            fn set(v: &str) -> HostVar {
                std::env::set_var(wsp_wire::host_tcp::ENV, v);
                HostVar
            }
        }
        impl Drop for HostVar {
            fn drop(&mut self) {
                std::env::remove_var(wsp_wire::host_tcp::ENV);
            }
        }

        /// The stuck seat mid-turn; every other seat unreadable.
        struct Seats;
        impl crate::cycle::Seats for Seats {
            fn state(&self, seat: &str) -> Option<State> {
                (seat == "cpd-t1").then_some(State::Working)
            }
        }

        fn ms(secs: i64) -> At {
            At(secs as u64 * 1000)
        }

        fn started(seat: &str, row: &str) -> Message {
            Message::SeatStarted(SeatStarted {
                key: Key { row: RowId(row.into()), attempt: 1 },
                seat: SeatId(seat.into()),
                epoch: Epoch(1),
                pid: 4242,
                sockets: vec![],
            })
        }

        /// A manager on loopback, the machine record naming it, and the
        /// daemon's fold thread on it; its first beat (its own probes, in this
        /// process) folded before anything is counted.
        fn manager(store: &Store, exec: &std::path::Path) -> (Manager, std::thread::JoinHandle<()>, std::sync::Arc<std::sync::atomic::AtomicBool>) {
            let mut cfg = Config::new(exec, HOST);
            cfg.port = 0;
            cfg.beat = Duration::from_secs(3600);
            let m = Manager::start(cfg).expect("the manager starts on loopback");
            let token = std::fs::read_to_string(exec.join(TOKEN_FILE)).unwrap();
            let local = crate::manager::Local {
                host: HOST.into(),
                addr: std::net::IpAddr::V4(std::net::Ipv4Addr::LOCALHOST),
                port: m.addrs()[0].port(),
                exec_dir: exec.to_path_buf(),
            };
            crate::manager::write_record(&store.state, &local, token.trim()).unwrap();
            let (thread, stop) = fold::start(store, None).expect("under WSP_HOST=manager with a record, the fold runs");
            let start = Instant::now();
            while !fold::beat_path(&store.state, HOST).exists() {
                assert!(start.elapsed() < Duration::from_secs(10), "no beat folded within 10s");
                std::thread::sleep(Duration::from_millis(50));
            }
            (m, thread, stop)
        }

        /// The scripted stream, folded by the daemon's thread: a seat started,
        /// mid-turn, its digest, stuck on itself, an order refused; a second
        /// seat started and stopped. Then the beat the sweeps read: one volume
        /// low, the load over the line.
        fn script(store: &Store, m: &Manager) {
            let now = util::epoch_secs();
            let t1 = SeatId("cpd-t1".into());
            let msgs = vec![
                started("cpd-t1", "wsp-t1"),
                Message::Turn(Turn { seat: t1.clone(), epoch: Epoch(1), edge: TurnEdge::Began, at: ms(now - 100), order: None }),
                Message::SeatDid(SeatDid {
                    seat: t1.clone(),
                    epoch: Epoch(1),
                    at: ms(now - 90),
                    digest: "- **ran:** cargo test fold:: · failed → passed\n- **read:** src/fold.rs".into(),
                    transcript: FileRef { host: HostId(HOST.into()), path: "/t.jsonl".into() },
                }),
                Message::SeatStuck(SeatStuck { seat: t1.clone(), epoch: Epoch(1), command: WAIT.into(), waited_secs: 80 }),
                Message::OrderRefused(OrderRefused { order: OrderId("o-1".into()), seat: t1.clone(), why: OrderFail::Refused }),
                started("cpd-t2", "wsp-t2"),
                Message::SeatStopped(SeatStopped { seat: SeatId("cpd-t2".into()), epoch: Epoch(1), exit: Some(0), ended: vec![4243], renderer_left: false }),
            ];
            let last = msgs.into_iter().map(|msg| m.emit(msg).unwrap()).last().unwrap();
            let start = Instant::now();
            while fold::read_folded(&store.state, HOST) != Some(last) {
                assert!(start.elapsed() < Duration::from_secs(10), "the stream was not folded within 10s");
                std::thread::sleep(Duration::from_millis(50));
            }
            let facts = HostFacts {
                load_centi: 4000,
                cpus: 4,
                memory: MemoryPressure::Normal,
                swap_used_bytes: 0,
                disks: vec![DiskFree { volume: "/Volumes/Scripted".into(), free_bytes: 10 << 30, total_bytes: 100 << 30 }],
                thermal: Thermal::Nominal,
                load5_centi: None,
                load15_centi: None,
                speed_limit_pct: None,
                hid_idle_secs: None,
                free_memory_bytes: None,
                pageouts: None,
            };
            let beat = Frame { id: None, req: None, msg: Message::Beat(Beat { at: ms(now), host: Some(facts), seats: vec![] }) };
            assert!(fold::fold(store, HOST, &beat));
        }

        /// One tick of the repair pass and its sweeps, as the daemon runs it;
        /// its line in the call log.
        fn tick(store: &Store) -> Value {
            wsp_base::calls::tick_begin();
            wsp_base::calls::phase("standing");
            repairs(store, &Seats, &mut crate::repair::Pass::new());
            wsp_base::calls::tick_end();
            let ticks: Vec<Value> = wsp_base::calls::read_all(&store.state)
                .into_iter()
                .filter(|l| l["k"] == "tick" && l["phases"].get("retro").is_some())
                .collect();
            assert_eq!(ticks.len(), 1, "{ticks:?}");
            ticks.into_iter().next().unwrap()
        }

        fn cycle_log(store: &Store) -> String {
            std::fs::read_to_string(store.state_file("cycle.log")).unwrap_or_default()
        }

        /// wsp-028's second leg, its done-when: under `manager` the tick runs
        /// against a loopback manager's scripted events; no `ps`, render or
        /// `df` (nor the throttle's probes) is in the call log for it;
        /// `repair:unsent` and `sweep:ended-panes` are gone; and disk,
        /// throttle, ending and stuck read the folded facts.
        #[test]
        fn under_the_manager_the_tick_reads_the_folded_events_and_probes_nothing() {
            let env = util::isolated("tick-manager");
            let _host = HostVar::set("manager");
            let store = Store::open();
            let exec = env.state().join("exec");
            let (m, thread, stop) = manager(&store, &exec);
            script(&store, &m);

            let line = tick(&store);
            for probe in PROBES {
                assert!(line["children"].get(probe).is_none(), "{probe} ran: {}", line["children"]);
            }
            for gone in ["repair:unsent", "sweep:ended-panes"] {
                assert!(line["phases"].get(gone).is_none(), "{gone} ran: {}", line["phases"]);
            }
            for kept in ["sweep:disk", "sweep:throttle", "ending:verification", "repair:refusing", "retro"] {
                assert!(line["phases"].get(kept).is_some(), "{kept} did not run: {}", line["phases"]);
            }

            // sweep:throttle: the beat's load, 40 on 4 cores, is over the line.
            let log = cycle_log(&store);
            assert!(log.contains("load is 40.0 (1m)") && log.contains("on 4 cores"), "{log}");
            // sweep:disk: the beat's volume, and only it.
            let disks = crate::disk::read(&store, crate::disk::LOW, false);
            assert_eq!(disks.iter().map(|d| (d.volume.as_str(), d.low())).collect::<Vec<_>>(), [("/Volumes/Scripted", true)]);
            // The stuck tick: the manager's `seat.stuck`, recorded and said.
            let stuck = crate::repair::stuck_seats(&store);
            assert_eq!(stuck.get("cpd-t1").map(|(k, _)| *k), Some(crate::stuck::Kind::SelfWait), "{stuck:?}");
            assert!(log.contains("cpd-t1: a command has been") && log.contains(WAIT), "{log}");
            // ending:verification: the seat's digest, not a transcript.
            let did = crate::cmd_agent::did_of_seat(&store, "cpd-t1", &util::iso_at(util::epoch_secs() - 600)).expect("the digest is read");
            assert_eq!((did.ran.len(), did.ran[0].last(), did.read.files.len()), (1, crate::did::Outcome::Passed, 1));
            assert!(crate::cmd_agent::did_of_seat(&store, "cpd-t1", &util::now_iso()).is_none(), "a digest before `since` is not read");

            // A second tick: the stall is the same one, said once.
            tick_again(&store);
            assert_eq!(cycle_log(&store).matches("cpd-t1: a command has been").count(), 1, "{}", cycle_log(&store));

            stop.store(true, Ordering::SeqCst);
            m.stop();
            let _ = thread.join();
        }

        /// The second tick, its line left uncounted by [`tick`]'s filter.
        fn tick_again(store: &Store) {
            wsp_base::calls::tick_begin();
            repairs(store, &Seats, &mut crate::repair::Pass::new());
            wsp_base::calls::tick_end();
        }
    }
}
