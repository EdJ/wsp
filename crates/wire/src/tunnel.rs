//! The tunnel supervisor: one ssh per executor, holding that machine's herdr
//! socket open on this one.
//!
//! This is the whole of the "servitor", and it lives on the *seat*. Nothing
//! long-lived of ours runs on the executor at all — over there it is a herdr
//! server, sshd, and a `wsp` shim that runs this machine's wsp back over the
//! same connection.
//!
//! The mechanism is one flag. `ssh -L <local.sock>:<remote.sock>` (OpenSSH
//! ≥ 6.7; we are on 9.9p2) puts the far machine's herdr socket at a path here,
//! and wsp's existing herdr client then speaks to it unmodified — no proxy, no
//! second protocol, no re-implementation of herdr's 89 methods. What tells the
//! two servers apart is the id: [`crate::herdr`] routes `w0:p3@mb2` to the
//! socket this file created.
//!
//! It lives in the daemon because the daemon is already the one long-lived
//! process, already has a tick, and already reloads itself when an `install`
//! lands underneath it. Nothing else should be opening connections: two
//! processes holding tunnels to one machine is two answers to "is mb2 up".

use std::collections::BTreeMap;
use std::path::PathBuf;
use std::process::Stdio;
use wsp_base::calls::{Child, Command};
use std::time::{Duration, Instant};

use crate::model::Machine;
use crate::store::{MachineLive, Store};
use crate::util;

/// The backoff, in seconds, by consecutive failure. The tail is flat at 15 s:
/// a machine that has just come back must be redialled within seconds of
/// answering (wsp-561, where the link was back at 12:47:54 and the tunnel read
/// `starting` until 12:53:11). What it costs a machine that stays off is one
/// ssh that fails at connect every 15 s; between ticks, [`Supervisor::recover`]
/// asks a cheaper question every [`RECOVER_EVERY`] and cuts the wait short
/// when the link answers.
const BACKOFF: [u64; 3] = [2, 5, 15];

/// How long a master may sit in `starting` — alive, and never answered — before
/// it is killed and dialled again at once. An ssh started during an outage can
/// stay up and never connect (stuck before kex, or on a route that has since
/// changed); without this it was held for as long as it lived (wsp-561).
const STARTING_WITHIN: Duration = Duration::from_secs(15);

/// How long a machine that answered is held reachable through failures that
/// reached the far side ([`crate::far::dead_link`] says no): a loaded machine
/// is slow, not gone. A failed connect ends the hold at once.
const SLOW_HOLD: Duration = Duration::from_secs(180);

/// How often, between the daemon's ticks, a machine that is not answering is
/// looked at again.
pub const RECOVER_EVERY: Duration = Duration::from_secs(5);

/// How long the cheap "is the link back" ssh may take.
const LINK_TEST_WITHIN: Duration = Duration::from_secs(8);

/// How often a reachable machine is asked what it holds for seats this store lacks.
const SWEEP_SECS: u64 = 600;

/// How long to wait after this many consecutive failures — the curve, on its
/// own, with no clock in it.
///
/// Separate from [`Tunnel::backoff`] for one reason: the curve is the thing
/// worth asserting and reading a clock to check it is what made the assertion
/// approximate. The test used to bracket `backoff` with two `Instant::now()`
/// calls and truncate the difference to whole seconds, so a scheduler gap of a
/// second between two adjacent statements read as a wrong answer — the fault
/// robustness-054 was filed for, in its cheapest form. Here it is arithmetic.
fn wait_after(failures: usize) -> Duration {
    Duration::from_secs(BACKOFF[failures.min(BACKOFF.len() - 1)])
}

struct Tunnel {
    child: Option<Child>,
    /// Consecutive failures, indexing [`BACKOFF`].
    failures: usize,
    /// When the next attempt is allowed. `None` means now.
    next_try: Option<Instant>,
    live: MachineLive,
    /// A `wsp machine check` this tick started and has not seen finish. The
    /// check starts an agent and takes seconds, so it runs beside the tick and
    /// never inside it.
    checking: Option<Child>,
    /// The master was just (re)started, so the seats' forwards on it are gone and
    /// are re-added once it answers (`docs/remote-agents.md` section 2).
    readd: bool,
    /// When the far seats are next beaten (`docs/remote-agents.md` section 3).
    next_beat: Option<Instant>,
    /// Per seat, what the last beat said — the failure's words, or empty for an
    /// answer — so `cycle.log` hears of a change and not of every beat.
    beat_said: BTreeMap<String, String>,
    /// When the machine is next asked what it holds for seats this store does not
    /// have (wsp-590). `None` is at once: a new master asks first thing.
    next_sweep: Option<Instant>,
    /// The strays already named in `cycle.log`, so each is said once while it stays.
    strays_said: std::collections::BTreeSet<String>,
    /// The machine and control path of the master this tunnel started, so
    /// [`Tunnel::stop`] can ask it to exit rather than only kill the ssh that
    /// was started.
    master: Option<(Machine, PathBuf)>,
    /// When the running master was started, and whether it has answered since:
    /// a master past [`STARTING_WITHIN`] that never has is replaced.
    started_at: Option<Instant>,
    master_answered: bool,
    /// When the machine last answered a probe, for [`SLOW_HOLD`].
    answered_at: Option<Instant>,
    /// The ssh program: `ssh`, or a script in a test.
    ssh: String,
    /// When this tunnel was last stepped, to measure the daemon's tick (wsp-639).
    last_step: Option<Instant>,
}

impl Tunnel {
    fn new() -> Tunnel {
        Tunnel {
            child: None,
            failures: 0,
            next_try: None,
            live: MachineLive { tunnel: "down".into(), ..Default::default() },
            checking: None,
            readd: false,
            next_beat: None,
            beat_said: BTreeMap::new(),
            next_sweep: None,
            strays_said: Default::default(),
            master: None,
            started_at: None,
            master_answered: false,
            answered_at: None,
            ssh: "ssh".into(),
            last_step: None,
        }
    }

    /// Note that a tick has reached this tunnel, and how long since the last:
    /// the beat goes out on the tick, so the gap between ticks is the slowest a
    /// beat can come, and [`crate::far::heard_is_stale_at`] allows for it. The
    /// slow gap is kept for a few ticks (half of it decays each fast tick), so a
    /// loaded daemon that catches up once does not read silent on the next lag.
    fn ticked(&mut self) {
        let gap = self.last_step.map_or(0, |t| t.elapsed().as_secs() as i64);
        self.last_step = Some(Instant::now());
        self.live.tick_secs = gap.max(self.live.tick_secs / 2);
    }

    /// Whether a failure that reached the far side keeps the machine reachable:
    /// it answered within [`SLOW_HOLD`].
    fn holds(&self) -> bool {
        self.live.reachable && self.answered_at.is_some_and(|a| a.elapsed() < SLOW_HOLD)
    }

    /// The link failed, for `why`. A dead link (a connect that failed) is
    /// unreachable; one that reached a slow far side is held reachable while
    /// [`Tunnel::holds`], and reads `slow`.
    fn link_failed(&mut self, why: String, word: &str) {
        let held = !crate::far::dead_link(&why) && self.holds();
        self.live.error = why;
        match held {
            true => self.live.tunnel = "slow".into(),
            false => {
                self.live.reachable = false;
                self.live.tunnel = word.into();
            }
        }
    }

    /// Kill the master that is running and forget it, without a backoff: the
    /// next step dials again.
    fn drop_master(&mut self) {
        if let Some((m, control)) = self.master.take() {
            clear_master_with(&self.ssh, &m, &control, MASTER_EXIT_WITHIN);
        }
        if let Some(mut child) = self.child.take() {
            let _ = child.kill();
            let _ = child.wait();
        }
        self.started_at = None;
        self.next_try = None;
    }

    /// Beat the machine's far seats when [`crate::far::beat_every`] has passed.
    /// The first beat after the master answers is at once, so a seat that was
    /// waiting on the link is let go the moment it is back.
    fn beat_if_due(&mut self, store: &Store, m: &Machine, wire: &dyn crate::far::Wire) -> Vec<(String, String)> {
        if self.next_beat.is_some_and(|t| Instant::now() < t) {
            return Vec::new();
        }
        self.next_beat = Some(Instant::now() + crate::far::beat_wait(m));
        let seats = crate::place_compound::beat_seats(store, &m.name);
        if seats.is_empty() {
            // No far seat to beat: the last beat belongs to a seat that is gone.
            crate::far::beat_idle(&mut self.live);
            return Vec::new();
        }
        // The controller's forward is dialled before it is used: one that is not
        // there (a master that was replaced, a daemon that restarted) is put back
        // rather than beaten into (wsp-475).
        if let Err(why) = ensure_controller(store, m, wire) {
            return seats.into_iter().map(|s| (s.id, why.clone())).collect();
        }
        crate::far::beat(m, &seats, wire, &mut self.live, &util::now_iso())
    }

    /// Whether [`Tunnel::beat_if_due`] would beat on this tick.
    fn beat_due(&self) -> bool {
        !self.next_beat.is_some_and(|t| Instant::now() < t)
    }

    /// One daemon tick of beating: beat when due, and say the result only for a
    /// tick that beat. A tick that did not beat has heard nothing from any seat,
    /// so it must not read as "answered" (wsp-475).
    fn beat_tick(&mut self, store: &Store, m: &Machine, wire: &dyn crate::far::Wire) {
        if !self.beat_due() {
            return;
        }
        let failed = self.beat_if_due(store, m, wire);
        self.say_beats(store, m, &failed);
    }

    /// Say in `cycle.log` what the beat's failures are, once per seat per change:
    /// a seat that stays deaf for the same reason is said once, and one that
    /// answers again after failing is said once too.
    fn say_beats(&mut self, store: &Store, m: &Machine, failed: &[(String, String)]) {
        let seats = crate::place_compound::beat_seats(store, &m.name);
        for s in &seats {
            let now = failed.iter().find(|(id, _)| id == &s.id).map(|(_, why)| why.clone()).unwrap_or_default();
            let before = self.beat_said.get(&s.id).cloned();
            if before.as_deref() == Some(now.as_str()) || (before.is_none() && now.is_empty()) {
                continue;
            }
            let line = match now.is_empty() {
                true => format!("wsp: {}: lease beat to {} answers again", m.name, s.id),
                false => format!("wsp: {}: lease beat to {} failed: {now}", m.name, s.id),
            };
            crate::store::log_line(store, &line);
            self.beat_said.insert(s.id.clone(), now);
        }
        self.beat_said.retain(|id, _| seats.iter().any(|s| &s.id == id));
    }

    /// Name, in `cycle.log`, what the machine holds for a seat this store does not
    /// have: a `compound-sup` session still running or a lease file left, after a
    /// seat was ended on this side and not there (wsp-590). Asked when the master
    /// is new and every [`SWEEP_SECS`] after; said once per stray while it stays.
    /// Named only: ending another seat's session is a person's to do.
    fn sweep_tick(&mut self, store: &Store, m: &Machine, wire: &dyn crate::far::Wire) {
        if self.next_sweep.is_some_and(|t| Instant::now() < t) || m.exec_dir.is_empty() {
            return;
        }
        self.next_sweep = Some(Instant::now() + Duration::from_secs(SWEEP_SECS));
        // A failed ask is the link's to say; the next sweep asks again.
        let Ok(out) = wire.run(m, &crate::far::holdings_command(m), crate::far::SESSION_TIMEOUT) else { return };
        let strays = crate::far::strays(&out, &crate::place_compound::seats_on(store, &m.name));
        for s in &strays {
            if self.strays_said.insert(s.label.clone()) {
                crate::store::log_line(store, &s.words(&m.name));
            }
        }
        self.strays_said.retain(|l| strays.iter().any(|s| &s.label == l));
    }

    /// Ask for a readiness check when there has been none, or the last one is
    /// old (`docs/remote-agents.md` section 7: "on a timer and on `machine add`"
    /// — a new machine has no check, so its first reachable tick is the one on
    /// `add`). Only for a machine whose wire matches: a mismatch is already the
    /// answer, and starting a session on it would be the thing `spawn --on`
    /// refuses.
    fn recheck(&mut self, store: &Store, m: &Machine) {
        if let Some(c) = self.checking.as_mut() {
            match c.try_wait() {
                Ok(None) => return,
                _ => self.checking = None,
            }
        }
        // What the last check wrote, before deciding whether it is stale: a
        // daemon that has just started knows nothing in memory, and asking for
        // a new check on every restart would be a model call per restart.
        carry_check(&mut self.live, store.machine_live(&m.name));
        if !wants_check(&self.live, m, crate::place_compound::wire_version(), util::now_iso().as_str()) {
            return;
        }
        let Ok(exe) = std::env::current_exe() else { return };
        let mut c = Command::new(exe);
        c.args(["machine", "check", &m.name]).stdin(Stdio::null());
        c.stdout(Stdio::null()).stderr(Stdio::null());
        self.checking = c.spawn().ok();
        // Marked as asked now, so a check that dies without writing does not
        // restart every tick; the file the check writes will overwrite this.
        self.live.checked = util::now_iso();
        self.live.ready = false;
        self.live.check_error = crate::far::IN_PROGRESS.into();
    }

    /// A master was just started: the seats' forwards on it are gone, and are put
    /// back once it answers ([`Tunnel::readd_if_due`]).
    fn started(&mut self, child: Child) {
        self.child = Some(child);
        self.readd = true;
        // A new master means the seats have been waiting on the link: the first
        // beat on it is at once.
        self.next_beat = None;
        self.next_sweep = None;
        self.started_at = Some(Instant::now());
        self.master_answered = false;
        // Not reachable yet: ssh has been started, not connected.
        // Saying otherwise here is how a machine reads as up for
        // the twenty seconds before anyone asks it anything — and
        // "up" is the word the reap guard is not allowed to be
        // wrong about. The one exception is a machine that answered
        // within the hold and whose master was lost to a slow far
        // side, not a failed connect: that is an answer, not a guess.
        if self.holds() {
            self.live.tunnel = "slow".into();
            return;
        }
        self.live.tunnel = "starting".into();
        self.live.reachable = false;
        self.live.error.clear();
    }

    /// The master answered: if it is a new one, put its forwards back — once.
    /// The flag is cleared after the attempt, so the next tick on the same
    /// master asks ssh for nothing.
    fn readd_if_due(&mut self, store: &Store, m: &Machine, wire: &dyn crate::far::Wire) {
        if self.readd {
            readd_forwards(store, m, wire);
            self.readd = false;
        }
    }

    fn backoff(&mut self) {
        self.next_try = Some(Instant::now() + wait_after(self.failures));
        self.failures += 1;
    }

    fn may_try(&self) -> bool {
        self.next_try.map(|t| Instant::now() >= t).unwrap_or(true)
    }

    /// Stop the ssh we started, if it is still running.
    ///
    /// Called on retire, on removal, and before the daemon `exec`s itself for a
    /// reload. That last one is not optional: an orphaned `ssh -L` goes on
    /// holding the socket it bound, and the daemon that comes up in our place
    /// cannot bind it — the tunnel would look permanently broken for a reason
    /// nothing on screen could explain.
    fn stop(&mut self) {
        // The master is asked to exit first, over its own socket: it is what an
        // install that drops the daemon would otherwise leave behind, and the
        // next tunnel would attach to it and exit 0.
        if let Some((m, control)) = self.master.take() {
            if self.child.is_some() {
                clear_master_with(&self.ssh, &m, &control, MASTER_EXIT_WITHIN);
            }
        }
        if let Some(mut child) = self.child.take() {
            let _ = child.kill();
            let _ = child.wait();
        }
        if let Some(mut c) = self.checking.take() {
            let _ = c.kill();
            let _ = c.wait();
        }
    }
}

/// Whether a readiness check is due: the machine answers, its wire is ours (a
/// mismatch is already the answer, and a session started on it is what
/// `spawn --on` refuses), there is an `exec_dir` to start one in, and there has
/// been no check or the last is older than [`crate::far::RECHECK_SECS`].
/// `now` is passed in so the rule has no clock in it.
fn wants_check(live: &MachineLive, m: &Machine, seat_wire: u32, now: &str) -> bool {
    if !live.reachable || !crate::far::accepts(live, seat_wire) || m.exec_dir.is_empty() {
        return false;
    }
    match live.checked.as_str() {
        "" => true,
        at => crate::util::epoch_of(now) - crate::util::epoch_of(at) >= crate::far::RECHECK_SECS as i64,
    }
}

/// Take the check's fields from the file. `wsp machine check` writes its result
/// to `machines.json` from another process and the tick rewrites the whole
/// record, so without this the result is gone before anyone reads it.
fn carry_check(live: &mut MachineLive, on_disk: Option<MachineLive>) {
    if let Some(d) = on_disk {
        live.ready = d.ready;
        live.checked = d.checked;
        live.check_error = d.check_error;
        live.shim_warning = d.shim_warning;
    }
}

pub struct Supervisor {
    tunnels: BTreeMap<String, Tunnel>,
    /// The ssh program each tunnel runs: `ssh`, or a script in a test.
    ssh: String,
}

impl Default for Supervisor {
    fn default() -> Supervisor {
        Supervisor { tunnels: BTreeMap::new(), ssh: "ssh".into() }
    }
}

impl Supervisor {
    pub fn new() -> Supervisor {
        Supervisor::default()
    }

    /// Whether any machine is not answering, so the daemon looks again between
    /// its ticks ([`Supervisor::recover`]).
    pub fn recovering(&self) -> bool {
        self.tunnels.values().any(|t| !t.live.reachable)
    }

    /// Between ticks, every [`RECOVER_EVERY`]: step each machine that is not
    /// answering, and only those. A master that is starting is probed again (so
    /// [`STARTING_WITHIN`] is kept to the second, not to the next 20 s tick); a
    /// tunnel waiting out its backoff is asked a cheap fresh `ssh … true` with no
    /// master, and is dialled now when it answers. That is what makes the redial
    /// within seconds of the link coming back, after an outage of any length.
    pub fn recover(&mut self, store: &Store) -> Vec<String> {
        let mut changed = Vec::new();
        for m in store.machines().into_iter().filter(|m| m.is_active()) {
            let Some(t) = self.tunnels.get_mut(&m.name) else { continue };
            if t.live.reachable {
                continue;
            }
            if t.child.is_none() && !t.may_try() {
                if link_answers(&t.ssh, &m).is_err() {
                    continue;
                }
                t.next_try = None;
            }
            let before = (t.live.reachable, t.live.tunnel.clone());
            let sock = store.machine_socket(&m.name);
            self.step(store, &m, &sock);
            if let Some(t) = self.tunnels.get_mut(&m.name) {
                if t.checking.is_none() {
                    carry_check(&mut t.live, store.machine_live(&m.name));
                }
                store.set_machine_live(&m.name, &t.live);
                if before != (t.live.reachable, t.live.tunnel.clone()) {
                    changed.push(m.name.clone());
                }
            }
        }
        changed
    }

    /// One pass: start what should be up, notice what has fallen over, and
    /// write down what is true.
    ///
    /// Returns the names whose state changed, for the daemon's `-v`. Cheap when
    /// there are no machines, which is the state every seat is in until
    /// somebody adds one — this whole file costs a directory read a tick until
    /// then.
    pub fn tick(&mut self, store: &Store) -> Vec<String> {
        let mut changed = Vec::new();
        let machines: Vec<Machine> = store.machines().into_iter().filter(|m| m.is_active()).collect();

        // Machines that have gone away, or been retired under us. Their
        // liveness goes with them: a record left behind would sit there
        // claiming a connection nothing is holding up.
        let wanted: Vec<&str> = machines.iter().map(|m| m.name.as_str()).collect();
        let stale: Vec<String> =
            self.tunnels.keys().filter(|n| !wanted.contains(&n.as_str())).cloned().collect();
        for name in stale {
            if let Some(mut t) = self.tunnels.remove(&name) {
                t.stop();
            }
            store.clear_machine_live(&name);
            changed.push(name);
        }

        for m in &machines {
            let sock = store.machine_socket(&m.name);
            let before = self.tunnels.get(&m.name).map(|t| (t.live.reachable, t.live.tunnel.clone()));
            self.step(store, m, &sock);
            let after = self.tunnels.get(&m.name).map(|t| (t.live.reachable, t.live.tunnel.clone()));
            if before != after {
                changed.push(m.name.clone());
            }
            if let Some(t) = self.tunnels.get_mut(&m.name) {
                // `wsp machine check` writes its result straight to the file,
                // from another process, and this record is rewritten whole
                // every tick — so take the check's fields from the file or the
                // result is gone before anyone reads it. Not while this tick
                // has one in flight: its placeholder is the newer fact.
                if t.checking.is_none() {
                    carry_check(&mut t.live, store.machine_live(&m.name));
                }
                store.set_machine_live(&m.name, &t.live);
            }
        }
        changed
    }

    fn step(&mut self, store: &Store, m: &Machine, sock: &std::path::Path) {
        let ssh = self.ssh.clone();
        let t = self.tunnels.entry(m.name.clone()).or_insert_with(|| Tunnel { ssh, ..Tunnel::new() });
        t.live.socket = sock.to_string_lossy().into_owned();
        t.ticked();

        // Has the ssh we started fallen over since last time?
        if let Some(child) = t.child.as_mut() {
            match child.try_wait() {
                Ok(Some(status)) => {
                    t.child = None;
                    t.started_at = None;
                    t.link_failed(why_it_died(store, &m.name, status.code()), "retrying");
                    t.backoff();
                    return;
                }
                Ok(None) => {}
                // We cannot tell whether it is running, which is not the same
                // as knowing it is not. Leave it alone rather than starting a
                // second one on top of the first.
                Err(_) => return,
            }
        }

        if t.child.is_none() {
            if !t.may_try() {
                return;
            }
            match spawn_with(&t.ssh, store, m, sock) {
                Ok(child) => {
                    let dir = sock.parent().unwrap_or(&store.state);
                    t.master = Some((m.clone(), dir.join(format!("cm-{}", m.name))));
                    t.started(child)
                }
                Err(e) => {
                    t.live.reachable = false;
                    t.live.tunnel = "retrying".into();
                    t.live.error = format!("cannot start ssh: {e}");
                    t.backoff();
                }
            }
            return;
        }

        // A live child is not a live machine. The socket exists from the
        // moment ssh binds it, whether or not anything is listening at the far
        // end, so the only honest test is to ask the far side a question: its
        // `compound-sup wire-version`, which is also the wire check
        // (`docs/remote-agents.md` section 2). A link that is down is
        // unreachable; a link that is up whose command failed is connected and
        // not ready — `far::fold_probe` keeps those two apart.
        //
        // The probe rides the master or nothing. ssh falls back to a connection
        // of its own when the ControlPath is not there, so a master still
        // connecting used to be probed past — answered by a direct ssh — or,
        // with the link down, read as `starting` for as long as it lived. The
        // master makes its socket once it is connected; until then it has not
        // answered.
        let control = control_path(store, &m.name);
        let probed = match control.exists() {
            true => crate::far::ssh_run_with(
                &t.ssh,
                m,
                Some(&control),
                &crate::far::probe_command(m),
                crate::far::PROBE_TIMEOUT,
            ),
            false => Err(crate::far::Failed::Link("the ssh master has not connected yet".into())),
        };
        let answered = !matches!(probed, Err(crate::far::Failed::Link(_)));
        match probed {
            Err(crate::far::Failed::Link(why)) => {
                // A master that has never answered, past its deadline, is
                // replaced now — the backoff is for a dial that failed, and this
                // one has not failed, it has stalled (wsp-561). A master that has
                // answered is left to its own keepalives: killing it for a slow
                // probe would drop every seat's forward on a loaded machine.
                if !t.master_answered && t.started_at.is_some_and(|s| s.elapsed() >= STARTING_WITHIN) {
                    t.drop_master();
                    t.link_failed(format!("no answer {}s after the master started: {why}", STARTING_WITHIN.as_secs()), "retrying");
                    return;
                }
                t.link_failed(why, "starting");
            }
            probed => {
                crate::far::fold_probe(&mut t.live, probed, &util::now_iso());
                t.master_answered = true;
                t.answered_at = Some(Instant::now());
            }
        }
        if answered {
            t.failures = 0;
            t.next_try = None;
            t.readd_if_due(store, m, &crate::far::Ssh);
            // After the re-add: a seat's socket is only there to dial once its
            // forward is back.
            t.beat_tick(store, m, &crate::far::Ssh);
            t.sweep_tick(store, m, &crate::far::Ssh);
        }
        t.recheck(store, m);
    }

    /// The machines that are answering right now.
    ///
    /// Out of what this pass found rather than out of `machines.json`, because
    /// the caller is the same tick that just wrote it and a read back would be
    /// the same fact with a chance of being staler.
    pub fn reachable(&self) -> Vec<String> {
        self.tunnels
            .iter()
            .filter(|(_, t)| t.live.reachable)
            .map(|(name, _)| name.clone())
            .collect()
    }

    /// Take every tunnel down. The daemon calls this before it `exec`s itself.
    pub fn shutdown(&mut self) {
        for t in self.tunnels.values_mut() {
            t.stop();
        }
    }
}

impl Drop for Supervisor {
    fn drop(&mut self) {
        self.shutdown();
    }
}

/// Where a machine's ssh writes its complaints.
fn err_log(store: &Store, name: &str) -> PathBuf {
    store.state.join("sock").join(format!("{name}.err"))
}

/// The longest a unix socket path may be, minus its terminator.
///
/// 104 bytes of `sun_path` on macOS, 108 on Linux, so the smaller one is the
/// rule. It is checked here rather than left to fail because ssh's answer is
/// `Bad local forwarding specification` with the whole path quoted back and no
/// mention of length — which reads as a syntax error in something wsp built,
/// and sends you reading the flags. Found by putting a store under a scratch
/// directory 116 characters deep.
pub const SUN_PATH_MAX: usize = 103;

/// The `ssh` we hold open, and why each flag is on it.
#[cfg(test)]
fn spawn(store: &Store, m: &Machine, sock: &std::path::Path) -> std::io::Result<Child> {
    spawn_with("ssh", store, m, sock)
}

/// [`spawn`] with the ssh program named.
fn spawn_with(ssh: &str, store: &Store, m: &Machine, sock: &std::path::Path) -> std::io::Result<Child> {
    let path = sock.to_string_lossy();
    if path.len() > SUN_PATH_MAX {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            format!(
                "socket path is {} bytes and a unix socket may be {SUN_PATH_MAX} — \
                 the state directory ({}) is too deep to forward from",
                path.len(),
                util::contract(&store.state),
            ),
        ));
    }
    let dir = sock.parent().unwrap_or(&store.state);
    std::fs::create_dir_all(dir)?;
    // ssh binds the local end and refuses a path that already exists — which,
    // after a daemon that was killed rather than stopped, it will.
    let _ = std::fs::remove_file(sock);

    let control = dir.join(format!("cm-{}", m.name));
    clear_master_with(ssh, m, &control, MASTER_EXIT_WITHIN);

    let log = std::fs::File::create(err_log(store, &m.name))?;

    let mut c = Command::new(ssh);
    c.args(ssh_args(m, &control));
    c.stdin(Stdio::null()).stdout(Stdio::null()).stderr(Stdio::from(log));
    c.spawn()
}

/// The cheap question asked while a machine is not answering: a fresh ssh, with
/// no master (`ControlPath=none`), that runs `true`. It is the same path every
/// other ssh to the machine takes, `~/.ssh/config` and proxies included, so its
/// answer is the one a person typing `ssh <machine>` would get.
fn link_answers(ssh: &str, m: &Machine) -> Result<(), String> {
    let mut c = Command::new(ssh);
    c.args(["-T", "-o", "BatchMode=yes", "-o", "ConnectTimeout=5", "-o", "ControlPath=none"]);
    c.arg(&m.ssh).args(["--", "true"]);
    match crate::far::run_bounded(c, LINK_TEST_WITHIN) {
        Ok(Some(o)) if o.status.success() => Ok(()),
        Ok(Some(o)) => Err(String::from_utf8_lossy(&o.stderr).trim().to_string()),
        Ok(None) => Err(format!("no answer in {}s", LINK_TEST_WITHIN.as_secs())),
        Err(e) => Err(format!("cannot start ssh: {e}")),
    }
}

/// How long a master gets to answer `-O exit` over its own socket. A live one
/// answers in milliseconds; this is the patience for one that is wedged.
const MASTER_EXIT_WITHIN: Duration = Duration::from_secs(5);

/// Close whatever master is still holding `control`, and sweep its path.
///
/// The supervisor only starts a master when it holds none, so one answering at
/// the path is somebody's leftover: an orphan from a daemon that dropped without
/// stopping (an `install` whose `launchctl bootstrap` failed, 2026-11-06). With
/// `ControlMaster=auto` the new ssh does not become the master, it attaches to
/// that one as a mux client, does nothing, and exits 0 — the machine reads
/// "ssh exited 0" for ever while the check passes. It is asked to exit over its
/// own control socket, by name and not by pattern; a path nothing answers on is
/// a plain headstone and is removed.
///
/// The ssh program and the deadline are named, so a test can pass a script and
/// the time it needs to start on a loaded machine; the tunnel passes its own
/// program and [`MASTER_EXIT_WITHIN`].
fn clear_master_with(ssh: &str, m: &Machine, control: &std::path::Path, within: Duration) {
    let _ = exit_master(ssh, m, control, within);
    let _ = std::fs::remove_file(control);
}

/// `ssh -O exit` on `control`: `true` when a master answered and was told to go.
fn exit_master(ssh: &str, m: &Machine, control: &std::path::Path, within: Duration) -> bool {
    if !control.exists() {
        return false;
    }
    let mut c = Command::new(ssh);
    c.args(["-S"]).arg(control).args(["-o", "BatchMode=yes", "-O", "exit"]).arg(&m.ssh);
    matches!(crate::far::run_bounded(c, within), Ok(Some(o)) if o.status.success())
}

/// Ask every ControlMaster this store's daemon left to exit, by socket and
/// not by pattern: the daemon is ended by `launchctl bootout` (a SIGTERM), which
/// never reaches `Supervisor::shutdown`, and the master `ssh` is not in the
/// job's process group in any way launchd waits for. An orphaned master blocks
/// the machine until someone closes it by hand (wsp-444, 2026-11-06).
pub fn reap_masters(state: &std::path::Path) {
    let Ok(dir) = std::fs::read_dir(state.join("sock")) else { return };
    for e in dir.flatten() {
        let name = e.file_name().to_string_lossy().to_string();
        if !name.starts_with("cm-") {
            continue;
        }
        let path = e.path();
        let mut c = Command::new("ssh");
        c.arg("-S").arg(&path).args(["-o", "BatchMode=yes", "-O", "exit", "wsp-reap"]);
        let _ = crate::far::run_bounded(c, Duration::from_secs(3));
        let _ = std::fs::remove_file(&path);
    }
}

/// Put back the forwards of every seat on `m`, read from the seat records: a
/// master that died took them with it. Each is added on its own and a failure of
/// one is no reason to skip the next (the stale local end is swept by
/// [`crate::far::Wire::forward`], so a re-add is idempotent).
fn readd_forwards(store: &Store, m: &Machine, wire: &dyn crate::far::Wire) {
    // The machine's controller first: the beat rides it, and a gate waits on the
    // beat. Forced, because the master is new and the old socket is a headstone.
    if let Err(why) = forward_controller(store, m, wire) {
        crate::store::log_line(store, &format!("wsp: {}: controller forward not restored: {why}", m.name));
    }
    for (local, far) in crate::place_compound::forwards_for(store, &m.name) {
        // ssh binds the local end and refuses a path that exists: the headstone
        // of the master that died. Swept here, before the wire is asked, so the
        // re-add does not depend on which wire it is.
        let _ = std::fs::remove_file(&local);
        let _ = wire.forward(m, "forward", &local, &far);
    }
}

/// Forward the machine's controller socket to its local end, when the machine
/// has far seats. The local end is swept first (ssh refuses a path that exists).
fn forward_controller(store: &Store, m: &Machine, wire: &dyn crate::far::Wire) -> Result<(), String> {
    if crate::place_compound::beat_seats(store, &m.name).is_empty() {
        return Ok(());
    }
    let far = crate::far::controller_far(m);
    if far.is_empty() {
        return Err("the machine has no exec_dir, so no controller socket to forward (wsp machine set <m> exec_dir=…)".into());
    }
    let local = crate::far::controller_local(store.machine_socket(&m.name).parent().unwrap_or(&store.state), &m.name);
    let _ = std::fs::remove_file(&local);
    wire.forward(m, "forward", &local, &far)
}

/// The controller forward, there when the beat needs it: dialled, and put back
/// when nothing answers at the local end.
fn ensure_controller(store: &Store, m: &Machine, wire: &dyn crate::far::Wire) -> Result<(), String> {
    let local = crate::far::controller_local(store.machine_socket(&m.name).parent().unwrap_or(&store.state), &m.name);
    if std::os::unix::net::UnixStream::connect(&local).is_ok() {
        return Ok(());
    }
    forward_controller(store, m, wire).map_err(|e| format!("the controller forward is not there and could not be added: {e}"))
}

/// The ControlMaster socket the tunnel's ssh holds, which the wire probe and
/// `wsp machine check` ride so they do not each open a connection.
pub fn control_path(store: &Store, name: &str) -> PathBuf {
    let sock = store.machine_socket(name);
    sock.parent().unwrap_or(&store.state).join(format!("cm-{name}"))
}

/// The ssh we hold open, and why each flag is on it.
fn ssh_args(m: &Machine, control: &std::path::Path) -> Vec<String> {
    let s = |x: &str| x.to_string();
    vec![
        // No command and no tty: this connection exists to carry a forward.
        s("-N"),
        s("-T"),
        // Never prompt. A daemon has no terminal, and a password prompt would
        // hold the connection open for ever instead of failing where we can
        // see it.
        s("-o"), s("BatchMode=yes"),
        // The one that must not be left off. Without it ssh connects happily
        // and reports success having silently failed to forward — so the
        // machine reads as connected and every call to it fails.
        s("-o"), s("ExitOnForwardFailure=yes"),
        s("-o"), s("ConnectTimeout=10"),
        // Notice a NAT timeout rather than hanging on a connection that is
        // already gone. Fallbacks, not overrides: `~/.ssh/config` is the user's
        // and anything set there for this host wins, which is how herdr's own
        // remote config behaves and for the same reason.
        s("-o"), s("ServerAliveInterval=15"),
        s("-o"), s("ServerAliveCountMax=3"),
        // One authenticated connection, reused by anything else ssh'ing to this
        // machine while we hold it. The master should be exactly as alive as
        // this process, so "is the tunnel up" has one answer and killing us is
        // enough to make it false.
        //
        // `ControlPersist=no` is spelled out rather than left to the default,
        // and it is not a fallback like the two above: the moment the user puts
        // `ControlPersist` in the `Host` block — which the README tells them to,
        // because every other ssh to an executor wants a warm connection — this
        // ssh forks into the background the instant it is started and the
        // foreground process exits 0. The forward is up and working; the
        // supervisor sees a child that died successfully, calls the machine
        // offline, backs off, and starts another one on top of it. Seen on the
        // first real executor, 2026-08-17, as `offline · ssh exited 0` against a
        // tunnel that was carrying traffic. -N is what makes it certain: with no
        // session to wait for, backgrounding is immediate.
        s("-o"), s("ControlMaster=auto"),
        s("-o"), s("ControlPersist=no"),
        s("-o"), format!("ControlPath={}", control.display()),
        // No `-L`: the master carries no forward of its own. Compound is two
        // sockets per seat, added with `ssh -O forward` on this master at spawn
        // and put back by [`readd_forwards`] (`docs/remote-agents.md` section 2),
        // so a machine-wide forward to herdr's socket, which compound replaced,
        // has nothing at its far end and nothing that reads it.
        m.ssh.clone(),
    ]
}

/// What ssh said on its way out, for the row that has to explain itself.
///
/// The exit status alone is no use — ssh exits 255 for everything from a
/// refused connection to a bad host key — so the last line it wrote is the
/// answer, and the status is the fallback for a death that said nothing.
fn why_it_died(store: &Store, name: &str, code: Option<i32>) -> String {
    let said = std::fs::read_to_string(err_log(store, name))
        .ok()
        .and_then(|s| s.lines().rev().find(|l| !l.trim().is_empty()).map(|l| l.trim().to_string()));
    match said {
        Some(line) => line,
        None => match code {
            Some(c) => format!("ssh exited {c}"),
            None => "ssh was killed".into(),
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The curve, and the flat tail. A machine that has just come back must not
    /// wait five minutes to be noticed (wsp-561: it waited 5 m 55 s), so the
    /// wait grows to 15 s and stops there.
    ///
    /// Asked of [`wait_after`] rather than of a `next_try` measured against a
    /// clock read a moment earlier: the curve is what is being asserted, and
    /// the reading only ever added a way for the answer to be wrong on a busy
    /// machine.
    #[test]
    fn the_backoff_grows_and_then_holds_at_fifteen_seconds() {
        let waits: Vec<u64> = (0..8).map(|n| wait_after(n).as_secs()).collect();
        assert_eq!(waits, [2, 5, 15, 15, 15, 15, 15, 15]);
        assert_eq!(wait_after(100_000), Duration::from_secs(15), "an outage of any length waits no longer (wsp-561)");

        // And that the tunnel spends it: nothing is tried until the wait it was
        // just given has passed.
        let mut t = Tunnel::new();
        assert!(t.may_try(), "a tunnel nobody has tried yet is tried now");
        for n in 0..8 {
            t.backoff();
            assert!(!t.may_try(), "and it is not tried again until then");
            assert_eq!(t.failures, n + 1, "a failure that was not counted is a wait not grown");
        }
    }

    /// A success clears the debt. Otherwise a machine that flapped this
    /// morning would still be on a minute's wait this afternoon, and the flap
    /// would cost more than the outage did.
    #[test]
    fn answering_puts_the_backoff_back_to_nothing() {
        let mut t = Tunnel::new();
        t.backoff();
        t.backoff();
        assert!(!t.may_try());

        t.failures = 0;
        t.next_try = None;
        assert!(t.may_try());
    }

    /// Started is not up. The socket exists from the moment ssh binds it,
    /// whether or not anything is listening at the far end — so a tunnel is
    /// born unreachable and only a real answer makes it otherwise. "Up" is the
    /// word the reap guard is not allowed to be wrong about.
    #[test]
    fn a_tunnel_is_not_reachable_until_something_has_answered() {
        let t = Tunnel::new();
        assert!(!t.live.reachable);
        assert_eq!(t.live.tunnel, "down");
        assert!(t.live.last_seen.is_empty(), "nothing has been seen yet");
    }

    /// The command line, because two of these flags are load-bearing and their
    /// absence is silent.
    ///
    /// `ExitOnForwardFailure` is the one that matters most: without it ssh
    /// connects, fails to forward, and stays up — so the machine reads as
    /// connected while every call to it fails, which is the worst of the three
    /// states to be wrong in. `BatchMode` is the other: a daemon has no
    /// terminal to answer a passphrase prompt at, and would hold the connection
    /// open waiting for one rather than failing where the row can say so.
    #[test]
    fn the_master_is_the_command_and_the_two_flags_that_must_be_on_it() {
        let mut m = Machine::new("mb2", "mac-mini");
        m.backend_at = "/Users/ed/.config/herdr/herdr.sock".into();
        let args = ssh_args(&m, std::path::Path::new("/s/sock/cm-mb2"));

        assert!(args.contains(&"ExitOnForwardFailure=yes".to_string()));
        assert!(args.contains(&"BatchMode=yes".to_string()));
        assert!(
            !args.contains(&"-L".to_string()),
            "the master forwards nothing of its own, and never herdr's socket: {args:?}",
        );
        assert_eq!(args.last().unwrap(), "mac-mini", "the Host alias, and it goes last");
        assert!(
            args.contains(&"ControlPersist=no".to_string()),
            "the master dies with us, and saying nothing here lets ~/.ssh/config say otherwise: {args:?}",
        );
    }

    /// A master left behind by a daemon that dropped without stopping is asked to
    /// exit over its own socket before a new one is started, and the path is
    /// swept either way. Seen 2026-11-06: the orphan held the control path, every
    /// new tunnel attached to it as a mux client and exited 0.
    #[test]
    fn a_stale_master_is_asked_to_exit_and_its_path_swept_before_a_new_one_starts() {
        use std::os::unix::fs::PermissionsExt;
        let dir = std::env::temp_dir().join(format!("wsp-tunnel-stale-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let said = dir.join("said");
        let fake = dir.join("ssh");
        std::fs::write(&fake, format!("#!/bin/sh\necho \"$@\" > {}\nexit 0\n", said.display())).unwrap();
        std::fs::set_permissions(&fake, std::fs::Permissions::from_mode(0o755)).unwrap();
        let control = dir.join("cm-mb2");
        std::fs::write(&control, "").unwrap();
        let m = Machine::new("mb2", "mac-mini");

        // The deadline is the test's own. The product's is MASTER_EXIT_WITHIN, and
        // what this test asks is what was said to the master, not how fast a
        // fresh `/bin/sh` starts: on a saturated machine (load 22 on 8 cores,
        // 0.4 % idle) a process sits in `_dyld_start` at 0 CPU for seconds before
        // its first line runs, and the 5 s deadline killed the script unread.
        let launch = Duration::from_secs(120);
        clear_master_with(fake.to_str().unwrap(), &m, &control, launch);

        let args = std::fs::read_to_string(&said).expect("the stale master was asked");
        assert!(args.contains(&format!("-S {}", control.display())), "{args}");
        assert!(args.contains("-O exit") && args.trim().ends_with("mac-mini"), "{args}");
        assert!(!control.exists(), "the path is swept");

        // Nothing at the path: nothing is asked, and nothing goes wrong.
        let _ = std::fs::remove_file(&said);
        clear_master_with(fake.to_str().unwrap(), &m, &control, launch);
        assert!(!said.exists(), "no master, no question");

        let _ = std::fs::remove_dir_all(&dir);
    }

    /// The limit that is not ours and does not announce itself. ssh answers a
    /// too-long forward path with `Bad local forwarding specification` and the
    /// path quoted back — which reads as a syntax error in something wsp built,
    /// and is really `sun_path` being 104 bytes on this platform. Caught here so
    /// the row says what is wrong rather than what ssh thought of it.
    #[test]
    fn a_socket_path_too_long_to_be_a_socket_says_so_before_ssh_does() {
        let deep = std::env::temp_dir().join("x".repeat(120));
        let store = Store::at(deep.clone(), deep.clone());
        let m = Machine::new("mb2", "mb2");
        let err = spawn(&store, &m, &store.machine_socket("mb2")).expect_err("too deep to forward");
        let said = err.to_string();
        assert!(said.contains(&SUN_PATH_MAX.to_string()), "{said}");
        assert!(said.contains("too deep"), "{said}");
        // And nothing was created on the way to finding out.
        assert!(!deep.exists());
    }

    /// ssh exits 255 for a refused connection, a bad host key and a dozen other
    /// things, so the status is the fallback and what it actually said is the
    /// answer.
    #[test]
    fn the_error_is_what_ssh_said_rather_than_what_it_returned() {
        let store = Store::at(
            std::env::temp_dir().join(format!("wsp-tun-{}", std::process::id())),
            std::env::temp_dir().join(format!("wsp-tun-{}/state", std::process::id())),
        );
        std::fs::create_dir_all(store.state.join("sock")).unwrap();

        assert_eq!(why_it_died(&store, "mb2", Some(255)), "ssh exited 255", "nothing written yet");
        assert_eq!(why_it_died(&store, "mb2", None), "ssh was killed");

        std::fs::write(
            err_log(&store, "mb2"),
            "Warning: something\nssh: connect to host mb2 port 22: No route to host\n\n",
        )
        .unwrap();
        assert_eq!(
            why_it_died(&store, "mb2", Some(255)),
            "ssh: connect to host mb2 port 22: No route to host",
            "the last thing it said, not the first",
        );

        let _ = std::fs::remove_dir_all(&store.root);
    }

    /// Records the forwards it is asked for, and whether the local end was still
    /// on disk when it was asked.
    struct Script {
        asked: std::cell::RefCell<Vec<(String, String, bool)>>,
        /// `(seat, state, socket file)` of each beat sent.
        leases: std::cell::RefCell<Vec<(String, String, String)>>,
        /// Seats whose beat gets no answer.
        deaf: std::cell::RefCell<Vec<String>>,
    }

    impl crate::far::Wire for Script {
        fn machine(&self, _: &str) -> Option<Machine> {
            None
        }
        fn run(&self, _: &Machine, _: &str, _: Duration) -> Result<String, crate::far::Failed> {
            Ok(String::new())
        }
        fn forward(&self, _: &Machine, op: &str, local: &std::path::Path, far: &str) -> Result<(), String> {
            assert_eq!(op, "forward");
            let name = local.file_name().unwrap().to_string_lossy().into_owned();
            self.asked.borrow_mut().push((name, far.to_string(), local.exists()));
            Ok(())
        }
        fn dir(&self, _: &Machine) -> PathBuf {
            PathBuf::new()
        }
        fn lease(&self, _: &Machine, socket: &std::path::Path, seat: &str, state: &str) -> Result<(), String> {
            let name = socket.file_name().unwrap().to_string_lossy().into_owned();
            self.leases.borrow_mut().push((seat.to_string(), state.to_string(), name));
            match self.deaf.borrow().contains(&seat.to_string()) {
                true => Err("no answer".into()),
                false => Ok(()),
            }
        }
    }

    fn script() -> Script {
        Script { asked: Default::default(), leases: Default::default(), deaf: Default::default() }
    }

    /// A store under a short scratch directory, with a far seat's record for each
    /// `(seat, machine)`: both sockets recorded, as `start` does.
    fn store_with_seats(name: &str, seats: &[(&str, &str)]) -> (Store, PathBuf) {
        let root = PathBuf::from("/tmp").join(format!("wt-{name}-{}-{}", std::process::id(), util::epoch_nanos() % 100_000));
        let _ = std::fs::remove_dir_all(&root);
        let store = Store::at(root.clone(), root.join("state"));
        let fwd = root.join("state/sock");
        std::fs::create_dir_all(&fwd).unwrap();
        for (seat, machine) in seats {
            let dir = store.state.join("compound-seats").join(seat);
            std::fs::create_dir_all(&dir).unwrap();
            let (sock, rsock) = crate::far::forward_paths(&fwd, seat);
            let rec = serde_json::json!({
                "machine": machine,
                "socket": sock.display().to_string(),
                "render_socket": rsock.display().to_string(),
                "far_socket": format!("/far/run/{seat}.sock"),
                "far_render_socket": format!("/far/run/{seat}.rsock"),
            });
            std::fs::write(dir.join("seat.json"), rec.to_string()).unwrap();
        }
        (store, root)
    }

    /// A far machine that says where its controller is.
    fn far_machine() -> Machine {
        let mut m = Machine::new("mb2", "mb2");
        m.exec_dir = "/far/exec".into();
        m
    }

    fn sleeper() -> Child {
        Command::new("sleep").arg("30").stdin(Stdio::null()).spawn().expect("sleep")
    }

    /// A master that dies takes its seats' forwards with it. Started again and
    /// answering, it gets both of a far seat's forwards back — once; the tick
    /// after asks ssh for nothing, until the master is replaced again.
    #[test]
    fn a_master_that_comes_back_gets_a_far_seats_two_forwards_once() {
        let (store, root) = store_with_seats("readd", &[("cpd-7", "mb2")]);
        let m = Machine::new("mb2", "mb2");
        let wire = script();
        let mut t = Tunnel::new();

        t.started(sleeper());
        assert!(t.readd, "a started master has lost the forwards");
        t.live.reachable = true;
        t.readd_if_due(&store, &m, &wire);
        let first: Vec<_> = wire.asked.borrow().iter().map(|(l, f, _)| (l.clone(), f.clone())).collect();
        assert_eq!(
            first,
            [
                ("cpd-7.rsock".to_string(), "/far/run/cpd-7.rsock".to_string()),
                ("cpd-7.sock".to_string(), "/far/run/cpd-7.sock".to_string()),
            ],
            "both the seat's sockets, each to its own far end",
        );

        t.readd_if_due(&store, &m, &wire);
        assert_eq!(wire.asked.borrow().len(), 2, "the next tick on the same master adds nothing");

        // The master drops (the step notices the exit and clears the child) and
        // a new one is started and answers.
        t.stop();
        t.started(sleeper());
        t.readd_if_due(&store, &m, &wire);
        assert_eq!(wire.asked.borrow().len(), 4, "a new master gets them again, once");
        t.readd_if_due(&store, &m, &wire);
        assert_eq!(wire.asked.borrow().len(), 4);

        t.stop();
        let _ = std::fs::remove_dir_all(&root);
    }

    /// The headstone of the dead master is swept before the re-add, so ssh is not
    /// refused a path that exists; and a seat on another machine is neither
    /// forwarded nor touched.
    #[test]
    fn a_stale_local_socket_is_swept_before_the_readd_and_another_machines_seat_is_left() {
        let (store, root) = store_with_seats("sweep", &[("cpd-7", "mb2"), ("cpd-8", "mb3")]);
        let fwd = root.join("state/sock");
        let (mine, mine_r) = crate::far::forward_paths(&fwd, "cpd-7");
        let (other, other_r) = crate::far::forward_paths(&fwd, "cpd-8");
        for p in [&mine, &mine_r, &other, &other_r] {
            std::fs::write(p, "").unwrap();
        }
        let m = Machine::new("mb2", "mb2");
        let wire = script();

        readd_forwards(&store, &m, &wire);

        let asked = wire.asked.borrow().clone();
        assert_eq!(asked.len(), 2, "only mb2's seat: {asked:?}");
        assert!(asked.iter().all(|(l, _, existed)| l.starts_with("cpd-7") && !existed), "swept before it was asked: {asked:?}");
        assert!(!mine.exists() && !mine_r.exists(), "the stale ends are gone");
        assert!(other.exists() && other_r.exists(), "mb3's seat is not touched");

        let _ = std::fs::remove_dir_all(&root);
    }

    fn due_machine() -> Machine {
        let mut m = Machine::new("mb2", "mb2");
        m.exec_dir = "~/exec".into();
        m
    }

    /// A check is asked for when there has been none or it is old, and only of a
    /// machine that answers, at our wire, with somewhere to start a session.
    #[test]
    fn a_check_is_due_when_there_has_been_none_or_the_last_is_old() {
        let w = 9;
        let up = MachineLive { reachable: true, wire: Some(w), ..Default::default() };
        let now = "2026-11-05T12:00:00Z";
        let m = due_machine();
        assert!(wants_check(&up, &m, w, now), "never checked");

        let fresh = MachineLive { checked: "2026-11-05T11:50:00Z".into(), ready: true, ..up.clone() };
        assert!(!wants_check(&fresh, &m, w, now), "ten minutes old is trusted");
        let old = MachineLive { checked: "2026-11-05T11:20:00Z".into(), ready: true, ..up.clone() };
        assert!(wants_check(&old, &m, w, now), "forty minutes old is asked again");

        assert!(!wants_check(&MachineLive { reachable: false, ..up.clone() }, &m, w, now), "unreachable");
        assert!(!wants_check(&MachineLive { wire: Some(w - 1), ..up.clone() }, &m, w, now), "wrong wire");
        assert!(!wants_check(&MachineLive { wire: None, ..up.clone() }, &m, w, now), "no wire");
        let mut bare = m.clone();
        bare.exec_dir.clear();
        assert!(!wants_check(&up, &bare, w, now), "nowhere to start a session");
    }

    /// The tick rewrites the record whole; the check's answer, written by
    /// another process, must survive it, and the probe's fields must not be
    /// replaced by the file's.
    #[test]
    fn the_checks_answer_survives_the_ticks_rewrite_of_the_record() {
        let mut live = MachineLive { reachable: true, wire: Some(9), last_seen: "now".into(), ..Default::default() };
        let on_disk = MachineLive {
            reachable: false,
            wire: Some(1),
            ready: true,
            checked: "2026-11-05T11:50:00Z".into(),
            check_error: "x".into(),
            shim_warning: "w".into(),
            ..Default::default()
        };
        carry_check(&mut live, Some(on_disk));
        assert!(live.ready && live.checked == "2026-11-05T11:50:00Z" && live.check_error == "x" && live.shim_warning == "w");
        assert!(live.reachable && live.wire == Some(9) && live.last_seen == "now", "the probe's facts are the tick's");

        carry_check(&mut live, None);
        assert!(live.ready, "no file, nothing to carry");
    }

    fn set_started(store: &Store, seat: &str, secs_ago: i64) {
        let path = store.state.join("compound-seats").join(seat).join("seat.json");
        let mut rec: serde_json::Value = serde_json::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
        let at = crate::util::epoch_of(&util::now_iso()) - secs_ago;
        rec["started_at"] = serde_json::json!(crate::util::iso_at(at));
        std::fs::write(path, rec.to_string()).unwrap();
    }

    /// The beat says `ok` to a seat something still holds and `stop` to one whose
    /// claim is gone, each on its own socket, to the machine's seats only: a
    /// local seat and another machine's seat hear nothing.
    #[test]
    fn the_beat_sends_ok_and_stop_to_the_machines_far_seats_and_to_no_others() {
        let (store, root) = store_with_seats("beat", &[("cpd-7", "mb2"), ("cpd-8", "mb2"), ("cpd-9", "mb3"), ("cpd-1", "")]);
        for s in ["cpd-7", "cpd-8", "cpd-9", "cpd-1"] {
            set_started(&store, s, 3600);
        }
        store.set_binding("cpd-7", serde_json::json!({ "task_id": "t-1" }));
        let m = far_machine();
        let wire = script();
        let mut t = Tunnel::new();

        let failed = t.beat_if_due(&store, &m, &wire);
        assert!(failed.is_empty(), "{failed:?}");
        assert_eq!(
            *wire.leases.borrow(),
            [
                ("cpd-7".to_string(), "ok".to_string(), "ctl-mb2.sock".to_string()),
                ("cpd-8".to_string(), "stop".to_string(), "ctl-mb2.sock".to_string()),
            ],
            "the held seat is told ok and the released one stop; mb3's seat and the local seat are not beaten",
        );
        assert!(!t.live.heard.is_empty(), "an answer is the machine heard");

        t.beat_if_due(&store, &m, &wire);
        assert_eq!(wire.leases.borrow().len(), 2, "not again before the next beat is due");
        t.started(sleeper());
        t.beat_if_due(&store, &m, &wire);
        assert_eq!(wire.leases.borrow().len(), 4, "a new master is beaten at once");
        t.stop();
        let _ = std::fs::remove_dir_all(&root);
    }

    /// A seat opened a moment ago has no claim yet and is not told to stop.
    #[test]
    fn a_seat_that_has_only_just_opened_is_told_ok_not_stop() {
        let (store, root) = store_with_seats("young", &[("cpd-7", "mb2")]);
        set_started(&store, "cpd-7", 5);
        let wire = script();
        Tunnel::new().beat_if_due(&store, &far_machine(), &wire);
        assert_eq!(wire.leases.borrow()[0].1, "ok");
        let _ = std::fs::remove_dir_all(&root);
    }

    /// Last-heard moves only on an answer: a beat nobody answers leaves it where
    /// it was, and one answer among several is enough to move it.
    #[test]
    fn last_heard_moves_only_when_a_beat_is_answered() {
        let (store, root) = store_with_seats("heard", &[("cpd-7", "mb2"), ("cpd-8", "mb2")]);
        let m = far_machine();
        let wire = script();
        *wire.deaf.borrow_mut() = vec!["cpd-7".into(), "cpd-8".into()];
        let mut t = Tunnel::new();
        t.live.heard = "2026-11-06T10:00:00Z".into();

        let failed = t.beat_if_due(&store, &m, &wire);
        assert_eq!(failed.len(), 2, "both said why: {failed:?}");
        assert_eq!(t.live.heard, "2026-11-06T10:00:00Z", "no answer, no change");

        t.next_beat = None;
        *wire.deaf.borrow_mut() = vec!["cpd-7".into()];
        let failed = t.beat_if_due(&store, &m, &wire);
        assert_eq!(failed.len(), 1);
        assert_ne!(t.live.heard, "2026-11-06T10:00:00Z", "one answer moves it");
        let _ = std::fs::remove_dir_all(&root);
    }

    /// A machine with no far seats is not beaten, and does not claim to have
    /// been heard.
    #[test]
    fn a_machine_with_no_far_seats_sends_nothing_and_hears_nothing() {
        let (store, root) = store_with_seats("none", &[("cpd-1", "")]);
        let wire = script();
        let mut t = Tunnel::new();
        t.beat_if_due(&store, &far_machine(), &wire);
        assert!(wire.leases.borrow().is_empty() && t.live.heard.is_empty());
        let _ = std::fs::remove_dir_all(&root);
    }
    /// A far session or lease file no seat here owns is named once in `cycle.log`
    /// and not ended; a seat's own, a check session and a lease temp are not
    /// named (wsp-590).
    #[test]
    fn a_far_session_and_lease_with_no_seat_are_named_once_and_left_alone() {
        struct Holds(std::cell::RefCell<Vec<String>>);
        impl crate::far::Wire for Holds {
            fn machine(&self, _: &str) -> Option<Machine> {
                None
            }
            fn run(&self, _: &Machine, cmd: &str, _: Duration) -> Result<String, crate::far::Failed> {
                self.0.borrow_mut().push(cmd.to_string());
                Ok("lease cpd-4070\nlease cpd-7\nlease .cpd-9.wsp.tmp\n\
                    /Users/e/bin/compound-sup spawn --label cpd-4070 --cwd /Users/e/wsp-exec/trees/demos-002 -- claude\n\
                    compound-sup spawn --label cpd-7 --cwd /x -- claude\n\
                    compound-sup spawn --label wsp-check-123 --cwd /x -- sleep 5\n"
                    .into())
            }
            fn forward(&self, _: &Machine, _: &str, _: &std::path::Path, _: &str) -> Result<(), String> {
                Ok(())
            }
            fn dir(&self, _: &Machine) -> PathBuf {
                PathBuf::new()
            }
            fn lease(&self, _: &Machine, _: &std::path::Path, _: &str, _: &str) -> Result<(), String> {
                Ok(())
            }
        }
        let (store, root) = store_with_seats("strays", &[("cpd-7", "mb2")]);
        let wire = Holds(Default::default());
        let m = far_machine();
        let mut t = Tunnel::new();
        t.sweep_tick(&store, &m, &wire);
        t.next_sweep = Some(Instant::now() - Duration::from_secs(1));
        t.sweep_tick(&store, &m, &wire);
        let log = cycle_log(&store);
        assert_eq!(log.matches("cpd-4070 holds a compound-sup session and a lease file").count(), 1, "{log}");
        assert!(!log.contains("cpd-7 holds") && !log.contains("wsp-check") && !log.contains(".cpd-9"), "{log}");
        let asked = wire.0.borrow().clone();
        assert!(asked.iter().all(|c| !c.contains("close") && !c.contains("rm ") && !c.contains("kill")), "named, not ended: {asked:?}");
        let _ = std::fs::remove_dir_all(&root);
    }

    fn cycle_log(store: &Store) -> String {
        std::fs::read_to_string(store.state_file("cycle.log")).unwrap_or_default()
    }

    /// The beat rides the machine's controller socket and never the seat's
    /// session socket; a new master gets the controller forward back before the
    /// seats' own, and a beat that finds it gone adds it again (wsp-475).
    #[test]
    fn the_beat_dials_the_controller_and_a_new_master_gets_it_back() {
        let (store, root) = store_with_seats("ctl", &[("cpd-7", "mb2"), ("cpd-8", "mb2")]);
        let m = far_machine();
        let wire = script();
        let mut t = Tunnel::new();

        let seats = crate::place_compound::beat_seats(&store, "mb2");
        assert!(seats.iter().all(|s| s.socket.file_name().unwrap() == "ctl-mb2.sock"), "{seats:?}");

        t.started(sleeper());
        t.live.reachable = true;
        t.readd_if_due(&store, &m, &wire);
        let asked: Vec<_> = wire.asked.borrow().iter().map(|(l, f, _)| (l.clone(), f.clone())).collect();
        assert!(asked.contains(&("ctl-mb2.sock".to_string(), "/far/exec/controller.msock".to_string())), "{asked:?}");
        assert_eq!(asked.len(), 5, "the controller once, and each seat's two: {asked:?}");

        // Nothing listens at the local end in a test, so the beat adds it again.
        t.beat_if_due(&store, &m, &wire);
        assert_eq!(wire.asked.borrow().len(), 6);
        assert_eq!(wire.leases.borrow().len(), 2);
        assert_eq!(t.live.leases.len(), 2, "the last answer per seat is kept");
        t.stop();
        let _ = std::fs::remove_dir_all(&root);
    }

    /// wsp-588: the last far seat ends, and the next beat forgets the old one, so
    /// the machine does not read `unreachable` off a beat nothing renews.
    #[test]
    fn a_beat_with_no_far_seat_left_forgets_the_last_one() {
        let (store, root) = store_with_seats("idle", &[("cpd-7", "mb2")]);
        let m = far_machine();
        let wire = script();
        let mut t = Tunnel::new();
        t.live.reachable = true;
        t.beat_if_due(&store, &m, &wire);
        assert!(!t.live.heard.is_empty() && t.live.leases.contains_key("cpd-7"));
        // The seat ends: its record goes.
        std::fs::remove_dir_all(store.state.join("compound-seats").join("cpd-7")).unwrap();
        t.live.heard = crate::util::iso_at(crate::util::epoch_secs() - 4000);
        t.next_beat = Some(Instant::now() - Duration::from_secs(1));
        t.beat_if_due(&store, &m, &wire);
        assert!(t.live.heard.is_empty() && t.live.leases.is_empty(), "{:?}", t.live);
        assert!(!crate::far::is_silent(&m, &t.live));
        let _ = std::fs::remove_dir_all(&root);
    }

    /// wsp-639: the tunnel measures the daemon's tick, so a loaded daemon's slow
    /// beat is allowed for, and the allowance stays a while after it catches up.
    #[test]
    fn a_slow_tick_is_measured_and_a_working_far_seat_does_not_read_silent_through_it() {
        let mut t = Tunnel::new();
        t.live.reachable = true;
        t.ticked();
        assert_eq!(t.live.tick_secs, 0, "the first tick has no gap");
        t.last_step = Some(Instant::now() - Duration::from_secs(78));
        t.ticked();
        assert_eq!(t.live.tick_secs, 78);
        t.live.heard = crate::util::iso_at(crate::util::epoch_secs() - 78);
        let mut m = far_machine();
        m.lease = Some(60);
        assert!(!crate::far::is_silent(&m, &t.live), "a beat 78 s ago on a 78 s tick");
        // Fast ticks bring the allowance down by halves, not at once.
        t.ticked();
        assert_eq!(t.live.tick_secs, 39);
        assert_eq!(MachineLive::from_value(&t.live.to_value()).tick_secs, 39, "and it is written where readers look");
    }

    /// A machine with no `exec_dir` cannot name its controller: every seat fails
    /// with that said, rather than the beat dialling something else.
    #[test]
    fn a_machine_without_exec_dir_fails_every_seat_and_says_why() {
        let (store, root) = store_with_seats("noexec", &[("cpd-7", "mb2")]);
        let wire = script();
        let failed = Tunnel::new().beat_if_due(&store, &Machine::new("mb2", "mb2"), &wire);
        assert_eq!(failed.len(), 1);
        assert!(failed[0].1.contains("exec_dir"), "{failed:?}");
        assert!(wire.leases.borrow().is_empty());
        let _ = std::fs::remove_dir_all(&root);
    }

    /// A beat that fails is said in `cycle.log` once per seat per change, and
    /// the seat answering again is said once.
    #[test]
    fn a_failed_beat_is_said_once_and_the_recovery_once() {
        let (store, root) = store_with_seats("said", &[("cpd-7", "mb2")]);
        let m = far_machine();
        let wire = script();
        *wire.deaf.borrow_mut() = vec!["cpd-7".into()];
        let mut t = Tunnel::new();
        // Ticks between beats: next_beat stays set, so only the first tick of
        // each beat period beats, as the daemon's does.
        for _ in 0..3 {
            for _ in 0..4 {
                t.beat_tick(&store, &m, &wire);
            }
            assert!(!cycle_log(&store).contains("answers again"), "{}", cycle_log(&store));
            t.next_beat = Some(Instant::now() - Duration::from_secs(1));
        }
        let log = cycle_log(&store);
        assert_eq!(log.matches("lease beat to cpd-7 failed: no answer").count(), 1, "{log}");
        assert!(!log.contains("answers again"), "{log}");
        wire.deaf.borrow_mut().clear();
        for _ in 0..2 {
            for _ in 0..4 {
                t.beat_tick(&store, &m, &wire);
            }
            t.next_beat = Some(Instant::now() - Duration::from_secs(1));
        }
        let log = cycle_log(&store);
        assert_eq!(log.matches("answers again").count(), 1, "{log}");
        assert_eq!(log.matches("lease beat to cpd-7 failed").count(), 1, "{log}");
        let _ = std::fs::remove_dir_all(&root);
    }

    /// A fake ssh for the supervisor: `down` in its directory makes every dial
    /// fail at connect, as a cut link does; `stall` makes a master that stays up
    /// and never connects (no control socket), as one started mid-outage can.
    /// Otherwise a master makes its control socket and stays up, the link test
    /// answers, and the probe prints a wire range. Each master and link test is
    /// a line in `said`.
    fn fake_ssh(dir: &std::path::Path) -> String {
        use std::os::unix::fs::PermissionsExt;
        let d = dir.display();
        let script = format!(
            r#"#!/bin/sh
case " $* " in *" -O "*) exit 0 ;; esac
if [ -e {d}/down ]; then echo "ssh: connect to host mb2 port 22: Operation timed out" >&2; exit 255; fi
case " $* " in
  *" -N "*)
    echo master >> {d}/said
    if [ ! -e {d}/stall ]; then
      for a in "$@"; do case "$a" in ControlPath=*) : > "${{a#ControlPath=}}" ;; esac; done
    fi
    exec sleep 300 ;;
  *" true "*) echo link >> {d}/said; exit 0 ;;
  *) echo "1 99" ;;
esac
"#
        );
        let fake = dir.join("ssh");
        std::fs::write(&fake, script).unwrap();
        std::fs::set_permissions(&fake, std::fs::Permissions::from_mode(0o755)).unwrap();
        fake.display().to_string()
    }

    fn said(dir: &std::path::Path, what: &str) -> usize {
        std::fs::read_to_string(dir.join("said")).unwrap_or_default().lines().filter(|l| *l == what).count()
    }

    /// Wait until `said` holds `n` lines of `what`. A master is a spawned child
    /// and writes its line when it gets the CPU, not when `spawn` returns; the
    /// patience is for that, as [`recover_until`]'s is.
    fn said_until(dir: &std::path::Path, what: &str, n: usize) {
        let give_up = Instant::now() + Duration::from_secs(120);
        while said(dir, what) < n {
            assert!(Instant::now() < give_up, "never {n} {what}: {}", said(dir, what));
            std::thread::sleep(Duration::from_millis(50));
        }
        assert_eq!(said(dir, what), n, "{what}");
    }

    /// A store with one machine, `mb2`, and a supervisor that dials it with the
    /// fake ssh.
    fn far_rig(name: &str) -> (Store, PathBuf, Supervisor) {
        let (store, root) = store_with_seats(name, &[]);
        store.save_machine(&Machine::new("mb2", "mb2")).unwrap();
        let mut sup = Supervisor::new();
        sup.ssh = fake_ssh(&root);
        (store, root, sup)
    }

    /// Step until `done` holds, by the recover path the daemon takes between
    /// ticks. The patience is for a fake `/bin/sh` to start on a loaded machine,
    /// not a wait the product makes: each pass is one slice.
    fn recover_until(sup: &mut Supervisor, store: &Store, done: impl Fn(&Tunnel) -> bool) {
        let give_up = Instant::now() + Duration::from_secs(120);
        while !done(&sup.tunnels["mb2"]) {
            assert!(Instant::now() < give_up, "never: {:?}", sup.tunnels["mb2"].live);
            sup.recover(store);
            std::thread::sleep(Duration::from_millis(50));
        }
    }

    /// The link comes back after an outage of any length, and the machine is
    /// redialled on the next slice between ticks — not when a backoff runs out
    /// (wsp-561: link back at 12:47:54, tunnel `starting` until 12:53:11). While
    /// the link is down, the slices ask the cheap question and dial nothing.
    #[test]
    fn a_far_link_that_comes_back_is_redialled_at_once_after_an_outage_of_any_length() {
        let (store, root, mut sup) = far_rig("back");
        std::fs::write(root.join("down"), "").unwrap();

        // The cut: the master fails at connect and the machine reads unreachable.
        sup.tick(&store);
        let give_up = Instant::now() + Duration::from_secs(120);
        while sup.tunnels["mb2"].live.tunnel != "retrying" {
            assert!(Instant::now() < give_up, "{:?}", sup.tunnels["mb2"].live);
            std::thread::sleep(Duration::from_millis(50));
            sup.tick(&store);
        }
        assert!(!sup.tunnels["mb2"].live.reachable);
        assert!(crate::far::dead_link(&sup.tunnels["mb2"].live.error), "{:?}", sup.tunnels["mb2"].live);
        assert!(sup.recovering());

        // An outage of any length: a week of failures, and a backoff not yet
        // spent. The cap holds the wait to 15 s however long it was down …
        let t = sup.tunnels.get_mut("mb2").unwrap();
        t.failures = 100_000;
        t.backoff();
        assert!(t.next_try.unwrap() <= Instant::now() + Duration::from_secs(15), "the backoff is capped");
        // … and the redial does not wait for even that.
        t.next_try = Some(Instant::now() + Duration::from_secs(3600));

        // Still down: the slice asks, and dials nothing.
        let masters = said(&root, "master");
        sup.recover(&store);
        assert!(sup.tunnels["mb2"].child.is_none(), "no master is dialled into a dead link");
        assert_eq!(said(&root, "master"), masters);
        assert!(!sup.tunnels["mb2"].live.reachable);

        // The link answers: the very next slice dials, with the backoff unspent.
        std::fs::remove_file(root.join("down")).unwrap();
        sup.recover(&store);
        assert_eq!(said(&root, "link"), 1, "the cheap question was asked");
        assert!(sup.tunnels["mb2"].child.is_some(), "and the master dialled in the same slice");
        // The master is a spawned child: its line in `said` comes when the fake
        // `/bin/sh` gets the CPU, which on a loaded machine is after `recover`.
        said_until(&root, "master", masters + 1);

        // And once its socket is there, the next slice finds it up.
        recover_until(&mut sup, &store, |t| t.live.reachable);
        let t = &sup.tunnels["mb2"];
        assert_eq!(t.live.tunnel, "up");
        assert_eq!(t.live.wire, Some(99));
        assert_eq!((t.failures, t.next_try), (0, None), "an answer clears the debt");
        assert!(!sup.recovering(), "and the daemon goes back to its one sleep");
        assert_eq!(store.machine_live("mb2").map(|l| l.reachable), Some(true), "written where `machine show` reads it");

        sup.shutdown();
        let _ = std::fs::remove_dir_all(&root);
    }

    /// A master that stays up and never connects is replaced once it has been
    /// `starting` for [`STARTING_WITHIN`], and the replacement is dialled at once:
    /// it stalled, it did not fail, so the backoff does not grow.
    #[test]
    fn a_far_master_that_never_connects_is_replaced_after_its_deadline_without_a_backoff() {
        let (store, root, mut sup) = far_rig("stall");
        std::fs::write(root.join("stall"), "").unwrap();
        sup.tick(&store);
        said_until(&root, "master", 1);

        // Inside its deadline it is left alone, and reads as starting.
        sup.recover(&store);
        let t = &sup.tunnels["mb2"];
        assert!(t.child.is_some() && t.live.tunnel == "starting" && !t.live.reachable, "{:?}", t.live);
        assert!(t.live.error.contains("not connected"), "{:?}", t.live);

        // Past it, it is killed, and nothing waits.
        let failures = t.failures;
        sup.tunnels.get_mut("mb2").unwrap().started_at = Some(Instant::now() - STARTING_WITHIN);
        sup.recover(&store);
        let t = &sup.tunnels["mb2"];
        assert!(t.child.is_none(), "the stalled master is gone");
        assert_eq!((t.failures, t.next_try), (failures, None), "redialled now, the backoff not grown");

        // The link is fine now: the next slice dials and the new master answers.
        std::fs::remove_file(root.join("stall")).unwrap();
        // The fake writes its line before its socket, so a master that reads
        // reachable has already said so.
        recover_until(&mut sup, &store, |t| t.live.reachable);
        assert_eq!(said(&root, "master"), 2);

        sup.shutdown();
        let _ = std::fs::remove_dir_all(&root);
    }

    /// A slow far side is not a dead link. A machine that answered within
    /// [`SLOW_HOLD`] stays reachable through a keepalive timeout, a late banner,
    /// and the master it loses to them being started again (wsp-561: load 34 on
    /// the i5 read `unreachable` for 2.5 min while ssh worked). A connect that
    /// fails ends the hold at once, and so does the hold running out.
    #[test]
    fn a_slow_far_probe_under_load_stays_reachable_and_a_failed_connect_does_not() {
        let mut t = Tunnel::new();
        t.live.reachable = true;
        t.live.tunnel = "up".into();
        t.answered_at = Some(Instant::now());

        t.link_failed("Timeout, server mb2 not responding.".into(), "retrying");
        assert!(t.live.reachable, "{:?}", t.live);
        assert_eq!(t.live.tunnel, "slow");
        t.started(sleeper());
        assert!(t.live.reachable && t.live.tunnel == "slow", "a new master on a slow machine: {:?}", t.live);
        t.link_failed("Connection timed out during banner exchange".into(), "starting");
        assert!(t.live.reachable, "{:?}", t.live);

        t.link_failed("ssh: connect to host mb2 port 22: Operation timed out".into(), "retrying");
        assert!(!t.live.reachable && t.live.tunnel == "retrying", "a failed connect is a dead link: {:?}", t.live);

        // A hold that has run out is no hold.
        t.live.reachable = true;
        t.answered_at = Some(Instant::now() - SLOW_HOLD);
        t.link_failed("Timeout, server mb2 not responding.".into(), "retrying");
        assert!(!t.live.reachable);
        // And a machine that never answered has nothing to hold.
        let mut fresh = Tunnel::new();
        fresh.link_failed("Timeout, server mb2 not responding.".into(), "retrying");
        assert!(!fresh.live.reachable);
        t.stop();
    }
}
