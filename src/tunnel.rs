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
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

use crate::model::Machine;
use crate::store::{MachineLive, Store};
use crate::util;

/// The backoff, in seconds, by consecutive failure. A machine that is switched
/// off should not be dialled every twenty seconds for a week; a machine that
/// has just come back should not wait five minutes to be noticed. The tail is
/// flat at a minute for that second reason.
const BACKOFF: [u64; 5] = [2, 5, 15, 30, 60];

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
    /// The machine and control path of the master this tunnel started, so
    /// [`Tunnel::stop`] can ask it to exit rather than only kill the ssh that
    /// was started.
    master: Option<(Machine, PathBuf)>,
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
            master: None,
        }
    }

    /// Beat the machine's far seats when [`crate::far::BEAT_SECS`] have passed.
    /// The first beat after the master answers is at once, so a seat that was
    /// waiting on the link is let go the moment it is back.
    fn beat_if_due(&mut self, store: &Store, m: &Machine, wire: &dyn crate::far::Wire) -> Vec<(String, String)> {
        if self.next_beat.is_some_and(|t| Instant::now() < t) {
            return Vec::new();
        }
        self.next_beat = Some(Instant::now() + Duration::from_secs(crate::far::BEAT_SECS));
        let seats = crate::place_compound::beat_seats(store, &m.name);
        if seats.is_empty() {
            return Vec::new();
        }
        crate::far::beat(m, &seats, wire, &mut self.live, &util::now_iso())
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
        self.live.tunnel = "starting".into();
        // Not reachable yet: ssh has been started, not connected.
        // Saying otherwise here is how a machine reads as up for
        // the twenty seconds before anyone asks it anything — and
        // "up" is the word the reap guard is not allowed to be
        // wrong about.
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
                clear_master(&m, &control);
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
    }
}

#[derive(Default)]
pub struct Supervisor {
    tunnels: BTreeMap<String, Tunnel>,
}

impl Supervisor {
    pub fn new() -> Supervisor {
        Supervisor::default()
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
        let t = self.tunnels.entry(m.name.clone()).or_insert_with(Tunnel::new);
        t.live.socket = sock.to_string_lossy().into_owned();

        // Has the ssh we started fallen over since last time?
        if let Some(child) = t.child.as_mut() {
            match child.try_wait() {
                Ok(Some(status)) => {
                    t.child = None;
                    t.live.reachable = false;
                    t.live.tunnel = "retrying".into();
                    t.live.error = why_it_died(store, &m.name, status.code());
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
            match spawn(store, m, sock) {
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
        let probed = crate::far::ssh_run(
            m,
            Some(&control_path(store, &m.name)),
            &crate::far::probe_command(m),
            crate::far::PROBE_TIMEOUT,
        );
        crate::far::fold_probe(&mut t.live, probed, &util::now_iso());
        if t.live.reachable {
            t.failures = 0;
            t.next_try = None;
            t.readd_if_due(store, m, &crate::far::Ssh);
            // After the re-add: a seat's socket is only there to dial once its
            // forward is back.
            let _ = t.beat_if_due(store, m, &crate::far::Ssh);
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
pub(crate) const SUN_PATH_MAX: usize = 103;

/// The `ssh` we hold open, and why each flag is on it.
fn spawn(store: &Store, m: &Machine, sock: &std::path::Path) -> std::io::Result<Child> {
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
    clear_master(m, &control);

    let log = std::fs::File::create(err_log(store, &m.name))?;

    let mut c = Command::new("ssh");
    c.args(ssh_args(m, &control));
    c.stdin(Stdio::null()).stdout(Stdio::null()).stderr(Stdio::from(log));
    c.spawn()
}

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
fn clear_master(m: &Machine, control: &std::path::Path) {
    clear_master_with("ssh", m, control);
}

/// [`clear_master`] with the ssh program named, so a test can pass a script.
fn clear_master_with(ssh: &str, m: &Machine, control: &std::path::Path) {
    let _ = exit_master(ssh, m, control);
    let _ = std::fs::remove_file(control);
}

/// `ssh -O exit` on `control`: `true` when a master answered and was told to go.
fn exit_master(ssh: &str, m: &Machine, control: &std::path::Path) -> bool {
    if !control.exists() {
        return false;
    }
    let mut c = Command::new(ssh);
    c.args(["-S"]).arg(control).args(["-o", "BatchMode=yes", "-O", "exit"]).arg(&m.ssh);
    matches!(crate::far::run_bounded(c, Duration::from_secs(5)), Ok(Some(o)) if o.status.success())
}

/// Put back the forwards of every seat on `m`, read from the seat records: a
/// master that died took them with it. Each is added on its own and a failure of
/// one is no reason to skip the next (the stale local end is swept by
/// [`crate::far::Wire::forward`], so a re-add is idempotent).
fn readd_forwards(store: &Store, m: &Machine, wire: &dyn crate::far::Wire) {
    for (local, far) in crate::place_compound::forwards_for(store, &m.name) {
        // ssh binds the local end and refuses a path that exists: the headstone
        // of the master that died. Swept here, before the wire is asked, so the
        // re-add does not depend on which wire it is.
        let _ = std::fs::remove_file(&local);
        let _ = wire.forward(m, "forward", &local, &far);
    }
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

    /// The curve, and the flat tail. A machine that is switched off must not be
    /// dialled every tick for a week, and one that has just come back must not
    /// wait five minutes to be noticed — so the wait grows and then stops
    /// growing.
    ///
    /// Asked of [`wait_after`] rather than of a `next_try` measured against a
    /// clock read a moment earlier: the curve is what is being asserted, and
    /// the reading only ever added a way for the answer to be wrong on a busy
    /// machine.
    #[test]
    fn the_backoff_grows_and_then_holds_at_a_minute() {
        let waits: Vec<u64> = (0..8).map(|n| wait_after(n).as_secs()).collect();
        assert_eq!(waits, [2, 5, 15, 30, 60, 60, 60, 60]);

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

        clear_master_with(fake.to_str().unwrap(), &m, &control);

        let args = std::fs::read_to_string(&said).expect("the stale master was asked");
        assert!(args.contains(&format!("-S {}", control.display())), "{args}");
        assert!(args.contains("-O exit") && args.trim().ends_with("mac-mini"), "{args}");
        assert!(!control.exists(), "the path is swept");

        // Nothing at the path: nothing is asked, and nothing goes wrong.
        let _ = std::fs::remove_file(&said);
        clear_master_with(fake.to_str().unwrap(), &m, &control);
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
            ..Default::default()
        };
        carry_check(&mut live, Some(on_disk));
        assert!(live.ready && live.checked == "2026-11-05T11:50:00Z" && live.check_error == "x");
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
        let m = Machine::new("mb2", "mb2");
        let wire = script();
        let mut t = Tunnel::new();

        let failed = t.beat_if_due(&store, &m, &wire);
        assert!(failed.is_empty(), "{failed:?}");
        assert_eq!(
            *wire.leases.borrow(),
            [
                ("cpd-7".to_string(), "ok".to_string(), "cpd-7.sock".to_string()),
                ("cpd-8".to_string(), "stop".to_string(), "cpd-8.sock".to_string()),
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
        Tunnel::new().beat_if_due(&store, &Machine::new("mb2", "mb2"), &wire);
        assert_eq!(wire.leases.borrow()[0].1, "ok");
        let _ = std::fs::remove_dir_all(&root);
    }

    /// Last-heard moves only on an answer: a beat nobody answers leaves it where
    /// it was, and one answer among several is enough to move it.
    #[test]
    fn last_heard_moves_only_when_a_beat_is_answered() {
        let (store, root) = store_with_seats("heard", &[("cpd-7", "mb2"), ("cpd-8", "mb2")]);
        let m = Machine::new("mb2", "mb2");
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
        t.beat_if_due(&store, &Machine::new("mb2", "mb2"), &wire);
        assert!(wire.leases.borrow().is_empty() && t.live.heard.is_empty());
        let _ = std::fs::remove_dir_all(&root);
    }
}
