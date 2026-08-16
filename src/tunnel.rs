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

use serde_json::json;

use crate::herdr;
use crate::model::Machine;
use crate::store::{MachineLive, Store};
use crate::util;

/// How long a machine gets to answer a liveness call. Short: this runs inside
/// the daemon's tick, and a machine that needs more than two seconds to list
/// its own workspaces over an established tunnel is not well.
const PROBE: Duration = Duration::from_secs(2);

/// The backoff, in seconds, by consecutive failure. A machine that is switched
/// off should not be dialled every twenty seconds for a week; a machine that
/// has just come back should not wait five minutes to be noticed. The tail is
/// flat at a minute for that second reason.
const BACKOFF: [u64; 5] = [2, 5, 15, 30, 60];

struct Tunnel {
    child: Option<Child>,
    /// Consecutive failures, indexing [`BACKOFF`].
    failures: usize,
    /// When the next attempt is allowed. `None` means now.
    next_try: Option<Instant>,
    live: MachineLive,
}

impl Tunnel {
    fn new() -> Tunnel {
        Tunnel {
            child: None,
            failures: 0,
            next_try: None,
            live: MachineLive { tunnel: "down".into(), ..Default::default() },
        }
    }

    fn backoff(&mut self) {
        let wait = BACKOFF[self.failures.min(BACKOFF.len() - 1)];
        self.failures += 1;
        self.next_try = Some(Instant::now() + Duration::from_secs(wait));
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
        if let Some(mut child) = self.child.take() {
            let _ = child.kill();
            let _ = child.wait();
        }
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
            if let Some(t) = self.tunnels.get(&m.name) {
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
                    t.child = Some(child);
                    t.live.tunnel = "starting".into();
                    // Not reachable yet: ssh has been started, not connected.
                    // Saying otherwise here is how a machine reads as up for
                    // the twenty seconds before anyone asks it anything — and
                    // "up" is the word the reap guard is not allowed to be
                    // wrong about.
                    t.live.reachable = false;
                    t.live.error.clear();
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
        // end, so the only honest test is to ask the far herdr a question.
        match herdr::call_on(Some(&m.name), "workspace.list", json!({}), PROBE) {
            Ok(_) => {
                t.failures = 0;
                t.next_try = None;
                t.live.reachable = true;
                t.live.tunnel = "up".into();
                t.live.last_seen = util::now_iso();
                t.live.error.clear();
            }
            Err(e) => {
                // The child is up and the far end is not answering. Do not
                // reach for "unreachable = empty" here either: this is written
                // down as not reachable, and the reap guard reads it that way.
                t.live.reachable = false;
                t.live.tunnel = "starting".into();
                t.live.error = e.to_string();
            }
        }
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
const SUN_PATH_MAX: usize = 103;

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

    let log = std::fs::File::create(err_log(store, &m.name))?;

    let mut c = Command::new("ssh");
    c.args(ssh_args(m, sock, &dir.join(format!("cm-{}", m.name))));
    c.stdin(Stdio::null()).stdout(Stdio::null()).stderr(Stdio::from(log));
    c.spawn()
}

/// The ssh we hold open, and why each flag is on it.
fn ssh_args(m: &Machine, sock: &std::path::Path, control: &std::path::Path) -> Vec<String> {
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
        // machine while we hold it. No `ControlPersist`: the master should be
        // exactly as alive as this process, so "is the tunnel up" has one
        // answer and killing us is enough to make it false.
        s("-o"), s("ControlMaster=auto"),
        s("-o"), format!("ControlPath={}", control.display()),
        // The whole mechanism, in one flag.
        s("-L"), format!("{}:{}", sock.display(), m.herdr_sock),
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
    #[test]
    fn the_backoff_grows_and_then_holds_at_a_minute() {
        let mut t = Tunnel::new();
        assert!(t.may_try(), "a tunnel nobody has tried yet is tried now");

        let mut waits = Vec::new();
        for _ in 0..8 {
            let before = Instant::now();
            t.backoff();
            let wait = t.next_try.unwrap().duration_since(before).as_secs();
            waits.push(wait);
            assert!(!t.may_try(), "and it is not tried again until then");
        }
        assert_eq!(waits, [2, 5, 15, 30, 60, 60, 60, 60]);
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
    fn the_forward_is_the_command_and_the_two_flags_that_must_be_on_it() {
        let mut m = Machine::new("mb2", "mac-mini");
        m.herdr_sock = "/Users/ed/.config/herdr/herdr.sock".into();
        let args = ssh_args(
            &m,
            std::path::Path::new("/s/sock/mb2.sock"),
            std::path::Path::new("/s/sock/cm-mb2"),
        );

        assert!(args.contains(&"ExitOnForwardFailure=yes".to_string()));
        assert!(args.contains(&"BatchMode=yes".to_string()));
        assert!(
            args.contains(&"/s/sock/mb2.sock:/Users/ed/.config/herdr/herdr.sock".to_string()),
            "the local socket first, the far machine's own path second: {args:?}",
        );
        assert_eq!(args.last().unwrap(), "mac-mini", "the Host alias, and it goes last");
        assert!(!args.iter().any(|a| a.contains("ControlPersist")), "the master dies with us");
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
}
