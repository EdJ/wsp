//! The daemon's launcher: a launchd user agent that `wsp install` writes and loads.
//!
//! # What this replaces, and what it was costing
//!
//! `wsp daemon` was started by backend's `[[startup]]` — one line in
//! the removed plugin manifest, and the whole of how a machine got a
//! daemon. backend is gone on this machine, so the daemon is gone with it:
//! `wsp watch --status` read `ticked 6d23h ago, the process is gone`, every
//! wake spool was still holding, and nothing anywhere said *why*. The first
//! symptom is silence, and silence from a watchdog is the one silence nobody
//! investigates.
//!
//! A launchd user agent is `wsp-134` option (a): `RunAtLoad` covers the login,
//! `KeepAlive` covers the crash and the `kill -9`, and — the part nothing else
//! on this machine had — it outlives the terminal the daemon was typed in. A
//! `nohup wsp daemon &` has none of those three, which is why the README had to
//! tell people to run it and why a week could pass with nobody doing.
//!
//! # Why the plist points at `~/.local/bin/wsp` and not at a build tree
//!
//! Because that is the file the daemon already replaces itself out of. `wsp
//! daemon` re-`exec`s the binary on disk within a tick of an install landing
//! (`crate::daemon::reload`), so pointing launchd at the installed file means a
//! reinstall is picked up by the same mechanism a panel's re-exec uses, and
//! there is no second place where the path a process started from is written
//! down and goes stale.
//!
//! # Why PATH is spelled out
//!
//! launchd hands a job `/usr/bin:/bin:/usr/sbin:/sbin` and nothing else, and
//! `~/.local/bin` is not on it — `path_helper` builds a login shell's PATH
//! from `/etc/paths`, and a launchd job does not run a login shell. The second
//! backend is found by `PATH` (`place_compound::Compound`'s census), so a
//! daemon launched without one is a daemon that cannot see half of this fleet.
//! `executor/backend-server.plist` argues the same thing for the executor's
//! plugins; this is the same argument, one file nearer the trunk.
//!
//! # What an install does about a job that is already there
//!
//! Nothing, when the plist on disk is byte for byte the one this machine wants
//! and launchd already has the job. The verb that could be used instead is
//! `launchctl kickstart -k`, and it is wrong twice over: it waits out the job's
//! `ThrottleInterval` before it will run anything again — 28 seconds measured
//! on a job that is running, 57 on one that has stopped, both of them an
//! install standing still — and there is nothing to restart for, because the
//! daemon replaces itself out of the file that was just installed within a
//! tick.
//!
//! When the plist *has* changed the pair `bootout` then `bootstrap` runs, and
//! both halves are needed. `bootstrap` on a loaded label fails with
//! `Input/output error`; and a job that is already loaded will never read the
//! file that was just written, which would leave a plist describing a daemon
//! launchd is not running.
//!
//! # Why a second daemon is still turned away
//!
//! backend's `[[startup]]` is not being removed here, only demoted: a machine
//! where `wsp install` has never run still gets a daemon when backend starts, and
//! that is the safety net for the machine this lands on. So launchd's job and
//! backend's can both be pointed at the same store, and `crate::daemon`'s one-
//! daemon-per-store rule is what makes that safe rather than a second copy of
//! t-260817-052. Nothing here coordinates with it, and nothing here should: the
//! refusal names the pid and `launchd` stays down, which is the outcome the
//! `KeepAlive` policy below is chosen for.
//!
//! # Why `KeepAlive` is not `true`
//!
//! Because `wsp daemon` exits **0** when it declines to run — a second daemon
//! on one store is not a failure, and backend's startup hook used to rely on
//! that. A plain `<true/>` would restart it immediately, it would decline
//! again, and launchd would spend the rest of the day running `wsp daemon` in a
//! loop. `SuccessfulExit: false` restarts a crash and a `kill -9` and leaves a
//! clean exit alone, and `ThrottleInterval` is here for the case the policy
//! still admits: a build that segfaults on startup, restarted every ten seconds
//! for ever, which is a machine nobody can install anything on.

use std::ffi::OsStr;
use std::path::{Path, PathBuf};
use wsp_base::calls::Command;
use std::time::Duration;

use crate::manager::Jobs;
use crate::util;

/// The job's name in launchd and in `~/Library/LaunchAgents`.
///
/// Not `edjames.wsp`, which is the backend plugin's id: this is a different
/// mechanism on a machine that may have several logins, and a launchd label is
/// what `launchctl print` and `bootout` take, so it has to be the string in the
/// plist and nowhere else.
pub const LABEL: &str = "com.wsp.daemon";

/// How long launchd waits before restarting a job that keeps dying. Without it
/// the default is ten seconds, which is fast enough to be noise in a log nobody
/// reads and slow enough to look like a daemon that keeps trying.
const THROTTLE: u32 = 30;

/// Where the plist goes. A *user* agent: a daemon on somebody's machine,
/// started at their login, and not something to be installed system-wide.
pub fn plist_path() -> PathBuf {
    util::home().join("Library/LaunchAgents").join(format!("{LABEL}.plist"))
}

/// [`plist_path`] under the name both launchers (this and [`crate::systemd`])
/// share, for the call sites that pick one by platform.
pub fn definition_path() -> PathBuf {
    plist_path()
}

/// [`plist`] under the shared name.
pub fn definition(bin: &Path, state: &Path, path_var: &str) -> String {
    plist(bin, state, path_var)
}

/// Is this instance a sandbox, where a launchd job would reach out of it?
///
/// The one place that decides, and it is the same test `cmd_install::install`
/// makes before it will replace a binary a sandbox does not own. Both exist
/// because `WSP_HOME` and `WSP_STATE` make a complete wsp instance and nothing
/// else: the daemon this writes would be pointed at `~/wsp` and at a socket
/// belonging to the live backend, and would then sync and reap against it, which
/// is the leak `robustness-021` was filed for. A throwaway `WSP_HOME` is the
/// most common way that happens.
pub fn sandboxed() -> bool {
    sandboxed_in(
        std::env::var_os("WSP_HOME").as_deref(),
        std::env::var_os("WSP_STATE").as_deref(),
        std::env::var_os("WSP_BIN").as_deref(),
    )
}

/// The same rule over values handed in, so it can be asked without the test
/// mutating this process's environment. `WSP_HOME` and `WSP_STATE` are the two
/// that make a complete wsp instance; `WSP_BIN` is what the launcher
/// checks before `~/.local/bin/wsp`, and its presence means the same thing here
/// — somebody handed this process a wsp that is not the machine's.
fn sandboxed_in(home: Option<&OsStr>, state: Option<&OsStr>, bin: Option<&OsStr>) -> bool {
    home.is_some() || state.is_some() || bin.is_some()
}

/// The `PATH` written into the plist: this process's, kept to directories that
/// exist, with `~/.local/bin` first.
///
/// The installer's, not a fixed list, because the thing being found is
/// `compound-sup` and `compound-render` and those live wherever this machine's
/// build put them. `~/.local/bin` goes first for the reason
/// [`crate::cmd_install::default_dest`] implies: it is where `wsp` itself goes,
/// and a job that found a different `wsp` would be a loop of the wrong one.
pub fn path_for(installer: Option<&OsStr>) -> String {
    let mut dirs: Vec<PathBuf> = vec![util::home().join(".local/bin")];
    if let Some(p) = installer {
        dirs.extend(std::env::split_paths(p).filter(|d| d.is_dir()));
    }
    dirs.retain(|d| d.is_dir());
    dirs.dedup();
    std::env::join_paths(dirs).unwrap_or_default().to_string_lossy().into_owned()
}

/// The plist `wsp install` writes, as text.
///
/// A function rather than a template file for the reason the rest of this
/// codebase builds things: the paths in it are facts about *this* machine, and
/// a template would leave three of them to be filled in by a caller with no way
/// to forget one.
pub fn plist(bin: &Path, state: &Path, path_var: &str) -> String {
    plist_as(LABEL, bin, state, path_var)
}

/// The same, for a named job. Split so a test can hand this module a label and
/// a directory of its own and drive the real launchd with them — which is the
/// only way the three `launchctl` calls below are ever going to be checked
/// against something that is not this machine's own daemon.
fn plist_as(label: &str, bin: &Path, state: &Path, path_var: &str) -> String {
    let log = state.join("daemon.log");
    let arg = |s: &str| format!("    <string>{}</string>", escape(s));
    format!(
        r#"<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
  <key>Label</key>
  <string>{label}</string>

  <key>ProgramArguments</key>
  <array>
{bin}
    <string>daemon</string>
  </array>

  <key>RunAtLoad</key>
  <true/>

  <key>KeepAlive</key>
  <dict>
    <key>SuccessfulExit</key>
    <false/>
  </dict>

  <key>ThrottleInterval</key>
  <integer>{throttle}</integer>

  <key>ProcessType</key>
  <string>Interactive</string>

  <key>EnvironmentVariables</key>
  <dict>
    <key>PATH</key>
{path}
  </dict>

  <key>StandardOutPath</key>
{log}
  <key>StandardErrorPath</key>
{log}
</dict>
</plist>
"#,
        label = label,
        bin = arg(&bin.display().to_string()),
        throttle = THROTTLE,
        path = arg(path_var),
        log = arg(&log.display().to_string()),
    )
}

/// XML text escaping for the four characters a path can hold. A home directory
/// with an `&` in it is not fanciful; a plist that will not parse is a job
/// launchd silently refuses, and there is no error from `bootstrap` worth
/// reading.
fn escape(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for c in s.chars() {
        match c {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '"' => out.push_str("&quot;"),
            _ => out.push(c),
        }
    }
    out
}

/// What writing the plist would do, worked out from what is on disk.
///
/// Split from the writing so the idempotence claim is testable without a
/// machine that has a launchd on it — and it is a claim worth testing, because
/// "writes a plist" run twice on a machine somebody also restarts is how a file
/// nobody can account for appears in `~/Library/LaunchAgents`.
#[derive(Debug, PartialEq)]
pub enum Plan {
    /// Nothing there, or what is there is not what this machine wants.
    Write,
    /// Byte for byte what is already there. Nothing is written and nothing is
    /// said about having written it.
    Unchanged,
}

pub fn plan(want: &str, path: &Path) -> Plan {
    match std::fs::read_to_string(path) {
        Ok(have) if have == want => Plan::Unchanged,
        _ => Plan::Write,
    }
}

/// The `gui/$UID` domain, which is where a user agent belongs.
///
/// `gui` rather than `system` for the reason `executor/backend-server.plist`
/// gives: it is the login session, and it is the only domain from which an
/// agent can reach the login keychain. `$UID` is asked of `id` rather than the
/// environment, because `UID` is a bash export and this has to work the same
/// from a `zsh` a person typed it in and from a hook that has neither.
fn domain() -> Result<String, String> {
    let out = Command::new("id")
        .arg("-u")
        .stderr(std::process::Stdio::null())
        .output()
        .map_err(|e| format!("cannot ask for this user's id: {e}"))?;
    if !out.status.success() {
        return Err("`id -u` did not answer".into());
    }
    let uid = String::from_utf8_lossy(&out.stdout).trim().to_string();
    if uid.is_empty() || !uid.chars().all(|c| c.is_ascii_digit()) {
        return Err(format!("`id -u` said {uid:?}, which is not a uid"));
    }
    Ok(format!("gui/{uid}"))
}

/// Is that job loaded right now?
///
/// Asked of `launchctl print`, which is the only question with an answer: a
/// plist on disk is a *description* of a job, and this is whether the job
/// exists. They come apart — a `bootout` on logout, a hand-deleted job in
/// launchd's own database — and the two of them being confused is how a machine
/// gets told its daemon is installed when nothing will start it.
///
/// **`target` is one argument, `gui/501/com.wsp.daemon`, and not two.** This is
/// the sharpest thing in the file and it is worth writing down because it
/// fails in the way that looks like success: `launchctl print gui/501
/// com.wsp.daemon` prints the *domain*, ignores the label, and exits **0**. So
/// a two-argument reading is a function that is always true — every install on
/// every machine would take the "already loaded" branch, `kickstart -k` would
/// fail with `Could not find service`, and the daemon would never be started by
/// anything while the plist sat in `~/Library/LaunchAgents` saying it had been.
/// The end-to-end test below is what caught it; `launchctl print` with two
/// arguments is the shape the manual check would also have missed, because the
/// command returns 0.
fn loaded(target: &str) -> bool {
    Command::new("launchctl")
        .arg("print")
        .arg(target)
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status()
        .map(|s| s.success())
        .unwrap_or(false)
}

/// What `wsp install` did about the launcher, in a sentence.
///
/// The whole of what happened, so that a person who ran an install and saw one
/// line about a binary now knows whether the daemon is covered — which is the
/// question the dead daemon cost a week to answer.
#[derive(Debug, PartialEq)]
pub struct Done {
    /// Where the plist is.
    pub path: PathBuf,
    /// Whether the file was rewritten. `false` on a second install, and the
    /// sentence says so rather than claiming a write that did not happen.
    pub wrote: bool,
    /// Whether the job was (re)loaded. `false` when the plist was already right
    /// and the job was already in the domain, which is the ordinary second
    /// install and is deliberately *nothing*: see [`ensure_as`].
    pub loaded: bool,
}

impl Done {
    pub fn line(&self) -> String {
        format!(
            "{} {} · {} — the daemon is a launchd agent now, and it comes back on its own",
            util::contract(&self.path),
            if self.wrote { "written" } else { "unchanged" },
            match self.loaded {
                true => "loaded, and it is running now",
                false => "already loaded, and it picks this binary up on its own",
            }
        )
    }
}

/// Write the plist if it is not already right, and make sure the job is loaded.
///
/// `bin` is the destination that was installed — the caller only ever gets here
/// with `~/.local/bin/wsp`, because an install with `--to` is a test of the
/// copy and has no business taking over the machine's daemon.
///
/// The binary is on disk and every pane has re-exec'd into it whatever launchd
/// does, but a daemon that is not running afterwards is **not** quiet any more
/// (wsp-449): this returns the error, the install prints it and exits non-zero,
/// and a note is kept for `wsp wip` until a daemon runs.
pub fn ensure(bin: &Path, state: &Path, path_var: &str) -> Result<Done, String> {
    if !cfg!(target_os = "macos") {
        return Err("launchd is macOS only; this platform's launcher is systemd".into());
    }
    ensure_as(LABEL, &plist_path(), bin, state, path_var)
}

/// [`ensure`], for a named job in a named place.
fn ensure_as(
    label: &str,
    path: &Path,
    bin: &Path,
    state: &Path,
    path_var: &str,
) -> Result<Done, String> {
    ensure_text(label, path, &plist_as(label, bin, state, path_var), false)
}

/// Write `want` at `path` if it is not already there, and make sure `label` is
/// loaded from it. `restart` is for a binary that was replaced under a job whose
/// plist did not change: the job is kicked so the new file runs, which is the
/// one case [`ensure`] leaves alone on purpose (the daemon re-`exec`s itself).
fn ensure_text(label: &str, path: &Path, want: &str, restart: bool) -> Result<Done, String> {
    let change = plan(want, path);
    let domain = domain()?;
    let target = format!("{domain}/{label}");

    // Already right and already there: nothing. Not an optimisation, and not
    // politeness — the verb that *could* be used here is measurably wrong.
    // `launchctl kickstart -k` on a job that is running waits out
    // `ThrottleInterval` first, measured on this machine at 28 seconds of an
    // install standing still, and 57 on a job that has stopped. And there is
    // nothing to restart for: the daemon re-`exec`s the file that was just
    // installed within a tick. **The daemon's pid is not touched** by an install
    // that changes nothing about the plist — that is the check the wsp-449 fix
    // is held to.
    let was_loaded = loaded(&target);
    if change == Plan::Unchanged && was_loaded {
        if restart {
            let out = Command::new("launchctl")
                .args(["kickstart", "-k", &target])
                .output()
                .map_err(|e| format!("cannot run launchctl: {e}"))?;
            if !out.status.success() {
                return Err(format!("launchctl kickstart failed: {}", String::from_utf8_lossy(&out.stderr).trim()));
            }
            settle(&target, Duration::from_secs(15))?;
            return Ok(Done { path: path.to_path_buf(), wrote: false, loaded: true });
        }
        return Ok(Done { path: path.to_path_buf(), wrote: false, loaded: false });
    }

    if change == Plan::Write {
        crate::store::write_atomic(path, want)
            .map_err(|e| format!("cannot write {}: {e}", util::contract(path)))?;
    }
    // Booted out only when it is there to be booted out: a job that is not
    // loaded has nothing to leave, and `bootout` of it is the call that used
    // to be made blind. When it is loaded the plist has changed, and a job that
    // is loaded never reads the file just written, so the pair is needed — but
    // `bootstrap` straight after `bootout` races the old process still
    // exiting, and that race is `launchctl bootstrap failed: 5: Input/output
    // error` (2026-11-06, twice). So: wait until the job is gone and its
    // process with it, then bootstrap, and bootstrap again on that error.
    if was_loaded {
        let _ = Command::new("launchctl")
            .arg("bootout")
            .arg(&target)
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .status();
        wait_gone(&target, Duration::from_secs(15));
    }
    bootstrap_retrying(&domain, path, 6, Duration::from_secs(1))?;
    settle(&target, Duration::from_secs(8))?;
    Ok(Done { path: path.to_path_buf(), wrote: change == Plan::Write, loaded: true })
}

// ---- the manager's agent (docs/agent-manager.md 2.4; wsp-024) ----------------

/// The manager's job: `<label>.manager`, beside the daemon's `com.wsp.daemon`.
pub const MANAGER_LABEL: &str = "com.wsp.manager";

/// How long launchd waits before restarting a manager that keeps dying (2.4).
/// Shorter than the daemon's: the manager owns no seat's life, and a client is
/// waiting on its port.
pub const MANAGER_THROTTLE: u32 = 5;

/// Where the manager's plist goes under `home`. A function of `home` rather
/// than of the environment, so a test writes into a temp HOME.
pub fn manager_plist_path_in(home: &Path) -> PathBuf {
    home.join("Library/LaunchAgents").join(format!("{MANAGER_LABEL}.plist"))
}

pub fn manager_plist_path() -> PathBuf {
    manager_plist_path_in(&util::home())
}

/// What the manager is, in the line `wsp install` prints.
pub const MANAGER_KIND: &str = "a launchd agent";

/// [`manager_plist_path`] under the name both launchers share
/// ([`crate::systemd::manager_definition_path`]).
pub fn manager_definition_path() -> PathBuf {
    manager_plist_path()
}

/// [`manager_plist`] under the name both launchers share
/// ([`crate::systemd::manager_definition`]).
pub fn manager_definition(bin: &Path, exec_dir: &Path, port: u16, jobs: &Jobs, path_var: &str) -> String {
    manager_plist(bin, exec_dir, port, jobs, path_var)
}

/// The manager's launchd log: `<exec_dir>/log/launchd.log`, beside the
/// manager's own `manager.log`. It holds what the process says before its own
/// log is open, and what it says as it dies.
pub fn manager_log(exec_dir: &Path) -> PathBuf {
    exec_dir.join("log").join("launchd.log")
}

/// The manager's plist as text. `KeepAlive` is a plain `true`, unlike the
/// daemon's: the manager has no clean exit to leave alone, and a manager that
/// declines to start (another one answers its port) is restarted every
/// [`MANAGER_THROTTLE`] seconds until the other is gone, which is what 2.4
/// asks for.
///
/// `--port` is written out, so the machine record and the process cannot
/// disagree about it. So are the job capacities and the tree root (`jobs`,
/// wsp-056): `--build` and `--check` from the throttle's slot settings, so the
/// manager's hard cap and wsp's own decision agree, and `--tree-root`.
pub fn manager_plist(bin: &Path, exec_dir: &Path, port: u16, jobs: &Jobs, path_var: &str) -> String {
    let arg = |s: &str| format!("    <string>{}</string>", escape(s));
    let jobs_args = jobs.args().iter().map(|a| arg(a)).collect::<Vec<_>>().join("\n");
    let log = manager_log(exec_dir);
    format!(
        r#"<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
  <key>Label</key>
  <string>{MANAGER_LABEL}</string>

  <key>ProgramArguments</key>
  <array>
{bin}
    <string>--exec-dir</string>
{exec_dir}
    <string>--port</string>
    <string>{port}</string>
{jobs_args}
  </array>

  <key>RunAtLoad</key>
  <true/>

  <key>KeepAlive</key>
  <true/>

  <key>ThrottleInterval</key>
  <integer>{MANAGER_THROTTLE}</integer>

  <key>LimitLoadToSessionType</key>
  <string>Aqua</string>

  <key>ProcessType</key>
  <string>Interactive</string>

  <key>EnvironmentVariables</key>
  <dict>
    <key>PATH</key>
{path}
  </dict>

  <key>StandardOutPath</key>
{log}
  <key>StandardErrorPath</key>
{log}
</dict>
</plist>
"#,
        bin = arg(&bin.display().to_string()),
        exec_dir = arg(&exec_dir.display().to_string()),
        path = arg(path_var),
        log = arg(&log.display().to_string()),
    )
}

/// Write the manager's plist at `path` and make its log directory, without
/// loading anything. What a test calls, and the first half of
/// [`ensure_manager`].
pub fn write_manager_plist(path: &Path, bin: &Path, exec_dir: &Path, port: u16, jobs: &Jobs, path_var: &str) -> Result<bool, String> {
    let want = manager_plist(bin, exec_dir, port, jobs, path_var);
    let _ = std::fs::create_dir_all(exec_dir.join("log"));
    if plan(&want, path) == Plan::Unchanged {
        return Ok(false);
    }
    crate::store::write_atomic(path, &want).map_err(|e| format!("cannot write {}: {e}", util::contract(path)))?;
    Ok(true)
}

/// Write the manager's plist if it differs and load the job. `restarted` says
/// the binary beside it was just replaced, so a job that is already right is
/// kicked to run it.
pub fn ensure_manager(bin: &Path, exec_dir: &Path, port: u16, jobs: &Jobs, path_var: &str, restarted: bool) -> Result<Done, String> {
    let _ = std::fs::create_dir_all(exec_dir.join("log"));
    ensure_text(
        MANAGER_LABEL,
        &manager_plist_path(),
        &manager_plist(bin, exec_dir, port, jobs, path_var),
        restarted,
    )
}

/// Unload the manager's job and remove its plist. Nothing to unload and
/// nothing to remove is success; the second value says whether there was a
/// plist.
pub fn remove_manager() -> Result<bool, String> {
    let path = manager_plist_path();
    let target = format!("{}/{MANAGER_LABEL}", domain()?);
    if loaded(&target) {
        let _ = Command::new("launchctl")
            .arg("bootout")
            .arg(&target)
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .status();
        wait_gone(&target, Duration::from_secs(15));
        if loaded(&target) {
            return Err(format!("{MANAGER_LABEL} is still loaded after bootout"));
        }
    }
    match std::fs::remove_file(&path) {
        Ok(()) => Ok(true),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(false),
        Err(e) => Err(format!("cannot remove {}: {e}", util::contract(&path))),
    }
}

/// Put the machine's daemon job back: `bootstrap` when launchd no longer has
/// it, `kickstart` when it has it and nothing is running. The caller has
/// already established that no daemon answers (`wsp daemon reload`), so this
/// never restarts a live one. Returns a sentence for what was done.
pub fn reload() -> Result<String, String> {
    if !cfg!(target_os = "macos") {
        return Err("launchd is macOS only; this platform's launcher is systemd".into());
    }
    let path = plist_path();
    if !path.exists() {
        return Err(format!("{} is not there — `wsp install --daemon` writes it", util::contract(&path)));
    }
    let domain = domain()?;
    let target = format!("{domain}/{LABEL}");
    let how = if loaded(&target) {
        let out = Command::new("launchctl")
            .args(["kickstart", &target])
            .output()
            .map_err(|e| format!("cannot run launchctl: {e}"))?;
        if !out.status.success() {
            return Err(format!("launchctl kickstart failed: {}", String::from_utf8_lossy(&out.stderr).trim()));
        }
        "kickstarted"
    } else {
        bootstrap_retrying(&domain, &path, 6, Duration::from_secs(1))?;
        "bootstrapped"
    };
    settle(&target, Duration::from_secs(8))?;
    Ok(format!("{LABEL} {how}"))
}

/// What `launchctl print` says the job is doing.
#[derive(Debug, PartialEq)]
pub enum State {
    /// Not in the domain at all.
    Absent,
    /// In the domain with a process.
    Running(u32),
    /// In the domain, no process, and the last exit status was this.
    Idle(Option<i32>),
}

/// Read a `launchctl print` body. Split from the call so it is testable
/// without a launchd.
pub fn state_from(print: &str) -> State {
    let mut pid = None;
    let mut exit = None;
    for l in print.lines() {
        let l = l.trim();
        if let Some(v) = l.strip_prefix("pid = ") {
            pid = v.trim().parse::<u32>().ok();
        } else if let Some(v) = l.strip_prefix("last exit code = ") {
            exit = v.trim().parse::<i32>().ok();
        }
    }
    match pid {
        Some(p) => State::Running(p),
        None => State::Idle(exit),
    }
}

fn state_of(target: &str) -> State {
    match Command::new("launchctl").arg("print").arg(target).output() {
        Ok(o) if o.status.success() => state_from(&String::from_utf8_lossy(&o.stdout)),
        _ => State::Absent,
    }
}

/// Wait for the job to leave the domain and its process to go.
fn wait_gone(target: &str, limit: Duration) {
    let start = std::time::Instant::now();
    while start.elapsed() < limit {
        if state_of(target) == State::Absent {
            return;
        }
        std::thread::sleep(Duration::from_millis(200));
    }
}

/// After `bootstrap`: the job is in the domain and has a process — or has run
/// and exited cleanly, which `KeepAlive: SuccessfulExit=false` does not
/// restart and which is a daemon that stood down on purpose. Anything else is
/// said, because "bootstrap returned 0" is not "the daemon is running".
fn settle(target: &str, limit: Duration) -> Result<(), String> {
    let start = std::time::Instant::now();
    let mut last = State::Absent;
    while start.elapsed() < limit {
        last = state_of(target);
        match last {
            State::Running(_) | State::Idle(Some(0)) => return Ok(()),
            _ => std::thread::sleep(Duration::from_millis(250)),
        }
    }
    Err(match last {
        State::Absent => "launchctl bootstrap returned, but `launchctl print` says the job is not loaded".into(),
        State::Idle(code) => format!(
            "the job is loaded but the daemon is not running (last exit {})",
            code.map(|c| c.to_string()).unwrap_or_else(|| "unknown".into())
        ),
        State::Running(_) => unreachable!(),
    })
}

/// [`bootstrap`], tried `tries` times while launchd answers `5: Input/output
/// error` — which it does when the old process is still going.
fn bootstrap_retrying(domain: &str, path: &Path, tries: u32, wait: Duration) -> Result<(), String> {
    let mut last = String::new();
    for n in 0..tries {
        match bootstrap(domain, path) {
            Ok(()) => return Ok(()),
            Err(why) => {
                let retry = why.contains("Input/output error") || why.contains(": 5:") || why.contains("exit 5");
                last = why;
                if !retry || n + 1 == tries {
                    break;
                }
                std::thread::sleep(wait);
            }
        }
    }
    Err(last)
}

/// Where an install that left the daemon not running says so, until one that
/// does not. `wsp wip` and the brief read it ([`unloaded_note`]).
fn unloaded_path(state: &Path) -> PathBuf {
    state.join("daemon-unloaded")
}

pub fn note_unloaded(state: &Path, why: &str) {
    let _ = std::fs::write(unloaded_path(state), format!("{why}\n"));
}

pub fn clear_unloaded(state: &Path) {
    let _ = std::fs::remove_file(unloaded_path(state));
}

/// The sentence for a daemon the last install could not leave running.
pub fn unloaded_note(state: &Path) -> Option<String> {
    let why = std::fs::read_to_string(unloaded_path(state)).ok()?;
    let why = why.trim();
    if why.is_empty() {
        return None;
    }
    Some(format!("the daemon is not running — the last `wsp install` could not leave it loaded: {why} (`wsp install --daemon` loads it)"))
}

/// Load the job, and turn a refusal into a sentence.
///
/// `bootstrap` is the one that fails informatively — a plist launchd will not
/// parse, a label already claimed in another domain, a filesystem it will not
/// read — and all of it comes back as one line on stderr. That line is the whole
/// difference between "the daemon is now covered" and "the daemon is now
/// covered in theory", so it is kept rather than replaced by an exit code.
fn bootstrap(domain: &str, path: &Path) -> Result<(), String> {
    let out = Command::new("launchctl")
        .arg("bootstrap")
        .arg(domain)
        .arg(path)
        .output()
        .map_err(|e| format!("cannot run launchctl bootstrap: {e}"))?;
    if out.status.success() {
        return Ok(());
    }
    let why = String::from_utf8_lossy(&out.stderr);
    let why = why.lines().next().unwrap_or("").trim();
    Err(format!(
        "launchctl bootstrap failed: {}",
        if why.is_empty() { format!("exit {}", out.status.code().unwrap_or(-1)) } else { why.to_string() }
    ))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    /// The manager's plist, written for a temp HOME and read back as text:
    /// launchd is never asked (wsp-024). The label, a plain `KeepAlive`, the
    /// 5 s throttle, the binary and the log under `<exec_dir>/log/`.
    #[test]
    fn the_managers_plist_is_written_under_a_temp_home_and_names_what_2_4_asks() {
        let home = scratch("manager-home");
        let exec = home.join(".local/state/wsp-manager");
        let bin = home.join(".local/bin/wsp-manager");
        let path = manager_plist_path_in(&home);
        assert!(path.starts_with(&home));
        assert!(path.ends_with("Library/LaunchAgents/com.wsp.manager.plist"));

        let jobs = Jobs { build: 3, check: 6, tree_root: "/Volumes/Work/src".into() };
        assert_eq!(write_manager_plist(&path, &bin, &exec, 7487, &jobs, "/usr/bin:/bin"), Ok(true));
        let got = std::fs::read_to_string(&path).unwrap();
        assert!(got.contains("<key>Label</key>\n  <string>com.wsp.manager</string>"), "{got}");
        assert!(got.contains("<key>KeepAlive</key>\n  <true/>"), "{got}");
        assert!(got.contains("<key>ThrottleInterval</key>\n  <integer>5</integer>"), "{got}");
        assert!(got.contains("<key>LimitLoadToSessionType</key>\n  <string>Aqua</string>"), "{got}");
        assert!(got.contains(&format!("<string>{}</string>\n    <string>--exec-dir</string>", bin.display())), "{got}");
        let log = exec.join("log/launchd.log");
        assert_eq!(got.matches(&format!("<string>{}</string>", log.display())).count(), 2, "{got}");
        assert!(exec.join("log").is_dir(), "the log's directory is made, or launchd cannot open the log");
        assert!(got.contains("<string>7487</string>"), "{got}");
        // The job capacities and the tree root the install chose (wsp-056).
        assert!(
            got.contains(
                "<string>--port</string>\n    <string>7487</string>\n    <string>--build</string>\n    <string>3</string>\n    \
                 <string>--check</string>\n    <string>6</string>\n    <string>--tree-root</string>\n    <string>/Volumes/Work/src</string>\n  </array>"
            ),
            "{got}"
        );

        assert_eq!(write_manager_plist(&path, &bin, &exec, 7487, &jobs, "/usr/bin:/bin"), Ok(false), "a second write is no write");
        let other = Jobs { build: 4, ..jobs };
        assert_eq!(write_manager_plist(&path, &bin, &exec, 7487, &other, "/usr/bin:/bin"), Ok(true), "other capacities are a write");
        let _ = std::fs::remove_dir_all(&home);
    }

    fn scratch(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("wsp-launchd-{}-{}", name, std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    /// A test job booted into the person's real `gui/<uid>` domain, booted out
    /// again however the test ends.
    ///
    /// The bootout used to be the test's last lines, so every assert above it
    /// that failed left `com.wsp.test-<pid>` loaded for good: wsp-149 found
    /// three in `launchctl list` on 2026-10-06, each pointing at a `fake-wsp`
    /// in a temp dir that no longer existed. Drop runs on an unwinding panic,
    /// which is what a failed assert is. It does not run on a run that is
    /// killed, and [`sweep`] is for that.
    #[cfg(target_os = "macos")]
    struct Job {
        target: String,
        dir: PathBuf,
    }

    #[cfg(target_os = "macos")]
    impl Drop for Job {
        fn drop(&mut self) {
            // Fails when the test already booted it out, which is the passing case.
            let _ = Command::new("launchctl")
                .args(["bootout", &self.target])
                .stdout(std::process::Stdio::null())
                .stderr(std::process::Stdio::null())
                .status();
            let _ = std::fs::remove_dir_all(&self.dir);
        }
    }

    /// Boot out every `com.wsp.test-<pid>` whose test process is gone, and
    /// remove every `wsp-launchd-agent-<pid>` scratch dir of the same.
    ///
    /// A `cargo test` stopped by a signal, or a Ctrl-C, never runs [`Job`]'s
    /// Drop, so the next run tidies up after it. Only the dead are touched: a
    /// label whose pid is running belongs to another tree's test run on this
    /// shared machine, mid-test, and booting it out would fail that run for
    /// no fault of its own.
    #[cfg(target_os = "macos")]
    fn sweep(domain: &str) {
        let Ok(out) = Command::new("launchctl").arg("list").output() else { return };
        let stale: Vec<(u32, String)> = String::from_utf8_lossy(&out.stdout)
            .lines()
            .filter_map(|l| l.split_whitespace().nth(2))
            .filter_map(|label| Some((label.strip_prefix("com.wsp.test-")?.parse().ok()?, label.to_string())))
            .collect();
        let tmp = std::env::temp_dir();
        let dirs: Vec<(u32, PathBuf)> = std::fs::read_dir(&tmp)
            .map(|rd| {
                rd.filter_map(|e| {
                    let name = e.ok()?.file_name().into_string().ok()?;
                    Some((name.strip_prefix("wsp-launchd-agent-")?.parse().ok()?, tmp.join(&name)))
                })
                .collect()
            })
            .unwrap_or_default();
        let pids: Vec<u32> = stale.iter().map(|(pid, _)| *pid).chain(dirs.iter().map(|(pid, _)| *pid)).collect();
        let running = crate::host::Host::alive(&crate::host::LOCAL, &pids);
        for (pid, label) in stale {
            if !running.contains(&pid) {
                let _ = Command::new("launchctl")
                    .args(["bootout", &format!("{domain}/{label}")])
                    .stdout(std::process::Stdio::null())
                    .stderr(std::process::Stdio::null())
                    .status();
            }
        }
        for (pid, dir) in dirs {
            if !running.contains(&pid) {
                let _ = std::fs::remove_dir_all(dir);
            }
        }
    }

    /// The `launchctl` calls, against a real launchd.
    ///
    /// Everything else in this file is a function over text, and the two calls
    /// that are not are the ones where a mistake shows up as `Input/output
    /// error` on somebody's machine at an install. So this drives the actual
    /// sequence — write, `bootstrap`, `print`, install again, `bootout` — for a
    /// job of its own name in a directory of its own, and checks that the thing
    /// which ran was `wsp daemon`.
    ///
    /// It earned its place on the first run. `launchctl print` was being given
    /// the domain and the label as two arguments, which prints the *domain*,
    /// ignores the label and exits **0** — so every install on every machine
    /// would have taken the "already loaded" branch and gone on to a
    /// `kickstart` of a job that does not exist, while the plist sat in
    /// `~/Library/LaunchAgents` saying it had been installed. macOS only:
    /// launchd is the thing under test.
    #[cfg(target_os = "macos")]
    #[test]
    fn the_job_is_written_loaded_and_the_second_install_leaves_it_alone() {
        let domain = domain().unwrap();
        sweep(&domain);
        let dir = scratch("agent");
        let label = format!("com.wsp.test-{}", std::process::id());
        let path = dir.join(format!("{label}.plist"));
        let _job = Job { target: format!("{domain}/{label}"), dir: dir.clone() };

        // Stands in for the daemon: a program that records what it was given and
        // exits cleanly — which is also the case the `KeepAlive` policy is
        // chosen for.
        let ran = dir.join("ran.log");
        let bin = dir.join("fake-wsp");
        wsp_testkit::script(&bin, &format!("#!/bin/sh\nprintf '%s\\n' \"$*\" >> {}\nexit 0\n", ran.display()));

        let first = ensure_as(&label, &path, &bin, &dir, "/usr/bin:/bin").expect("the first install");
        assert!(first.wrote, "nothing was written on the first run");
        assert!(first.loaded, "a job that was not loaded was not bootstrapped");
        assert!(loaded(&format!("{domain}/{label}")), "the job did not survive being written");

        // `RunAtLoad`, and the `daemon` argument, actually reaching the program.
        // Twenty seconds because launchd's first exec of a script written a
        // moment ago has been seen to miss five on a loaded machine (wsp-339:
        // one failure in a full-suite run, not reproduced in 25 isolated runs
        // under 8 busy loops or in three later suite runs); a pass breaks out
        // on the first sight of the log, so the longer wait costs nothing.
        // Waited for by what it says, not by the file existing: `>>` creates the
        // log before `printf` writes to it, and on a saturated machine the gap
        // is long enough to read an empty file (seen 2026-11-06, `log Ok("")`).
        for _ in 0..400 {
            if std::fs::read_to_string(&ran).is_ok_and(|t| t.contains("daemon")) { break }
            std::thread::sleep(Duration::from_millis(50));
        }
        let ran_text = std::fs::read_to_string(&ran);
        if !ran_text.as_deref().unwrap_or_default().contains("daemon") {
            let said = Command::new("launchctl").args(["print", &format!("{domain}/{label}")]).output().unwrap();
            panic!(
                "the job ran something other than `wsp daemon`: log {ran_text:?}, script exists {}, launchctl says:\n{}",
                bin.exists(),
                String::from_utf8_lossy(&said.stdout)
            );
        }

        // The ordinary install: same text, same job. Nothing, because
        // `kickstart -k` on a running job waits out `ThrottleInterval` first —
        // measured at 28 seconds of an install standing still — and there is no
        // restart to do, the daemon re-execs what was just installed by itself.
        let second = ensure_as(&label, &path, &bin, &dir, "/usr/bin:/bin").expect("the second install");
        assert!(!second.wrote, "an identical plist was rewritten");
        assert!(!second.loaded, "a job that was already loaded was booted out for nothing");
        assert!(loaded(&format!("{domain}/{label}")), "and the job did not survive that either");

        // A plist that *did* change is reloaded, because `bootstrap` on a
        // loaded label fails outright and nothing else would ever read the new
        // file.
        std::fs::write(&path, "<!-- edited by hand -->").unwrap();
        let third = ensure_as(&label, &path, &bin, &dir, "/usr/bin:/bin").expect("the third install");
        assert!(third.wrote, "a hand-edited plist was left alone");
        assert!(third.loaded, "and the job was never told about it");

        let out = Command::new("launchctl")
            .args(["bootout", &format!("{domain}/{label}")])
            .output()
            .unwrap();
        assert!(out.status.success(), "bootout failed: {}", String::from_utf8_lossy(&out.stderr));
        // `bootout` returns before the job is gone when the machine is busy
        // (the full suite, 3 runs of 3), so look again for up to 20 s, as the
        // log file above is waited for, and say what launchctl still says.
        let target = format!("{domain}/{label}");
        let mut last = String::new();
        for _ in 0..100 {
            let said = Command::new("launchctl").args(["print", &target]).output().unwrap();
            if !said.status.success() { last.clear(); break }
            last = String::from_utf8_lossy(&said.stdout).to_string();
            std::thread::sleep(Duration::from_millis(200));
        }
        assert!(last.is_empty(), "the job outlived its bootout by 20 s, launchctl still says:\n{last}");
    }

    /// What `launchctl print` says is read as a state, so "loaded" is no longer
    /// taken for "running" (wsp-449).
    #[test]
    fn a_print_body_is_read_as_running_or_idle() {
        assert_eq!(state_from("x = {\n\tstate = running\n\tpid = 4242\n}"), State::Running(4242));
        assert_eq!(state_from("x = {\n\tstate = not running\n\tlast exit code = 0\n}"), State::Idle(Some(0)));
        assert_eq!(state_from("x = {\n\tstate = spawn scheduled\n}"), State::Idle(None));
    }

    /// An install that could not leave the daemon running leaves a line for
    /// `wip`, and one that did takes it away.
    #[test]
    fn the_unloaded_note_is_kept_until_a_daemon_runs() {
        let dir = scratch("note");
        assert_eq!(unloaded_note(&dir), None);
        note_unloaded(&dir, "launchctl bootstrap failed: 5: Input/output error");
        let said = unloaded_note(&dir).expect("a note");
        assert!(said.contains("not running") && said.contains("Input/output error"), "{said}");
        clear_unloaded(&dir);
        assert_eq!(unloaded_note(&dir), None);
    }

    /// The three things the plist is for, asserted as the plist rather than as
    /// a launchd that has to be asked: it starts at login, it comes back after
    /// a crash, and it runs the *installed* binary rather than a build tree —
    /// the last of which is what keeps a reinstall picked up by the same
    /// `exec` every panel uses.
    ///
    /// `KeepAlive` is read as a predicate rather than as `<true/>` because the
    /// difference between `<true/>` and `SuccessfulExit: false` is a machine
    /// spending the day restarting a daemon that declined to run, and only one
    /// of the two survives a person installing twice.
    #[test]
    fn the_plist_starts_the_installed_daemon_at_login_and_brings_it_back() {
        let got = plist(
            Path::new("/Users/x/.local/bin/wsp"),
            Path::new("/Users/x/.local/state/wsp"),
            "/Users/x/.local/bin:/usr/bin:/bin",
        );
        assert!(got.contains("<key>RunAtLoad</key>\n  <true/>"), "{got}");
        assert!(
            got.contains("<key>KeepAlive</key>\n  <dict>\n    <key>SuccessfulExit</key>\n    <false/>"),
            "a plain KeepAlive would restart a daemon that declined to run, for ever: {got}"
        );
        assert!(
            got.contains("<string>/Users/x/.local/bin/wsp</string>\n    <string>daemon</string>"),
            "it runs something other than the installed binary: {got}"
        );
        assert!(
            got.contains("<string>/Users/x/.local/state/wsp/daemon.log</string>"),
            "nowhere for the log to go: {got}"
        );

        // `~/Library/LaunchAgents` and `gui/$UID`, because a system domain or a
        // system path is the wrong machine for a person's daemon.
        assert!(plist_path().ends_with(Path::new("Library/LaunchAgents/com.wsp.daemon.plist")));
        assert!(domain().unwrap().starts_with("gui/"), "{:?}", domain());
    }

    /// A path is not XML. A home directory with an ampersand in it produces a
    /// plist launchd refuses to parse, and `bootstrap` reports that in one line
    /// that names neither the file nor the reason — so the only place to get it
    /// right is here.
    #[test]
    fn a_path_with_the_wrong_characters_in_it_still_parses() {
        let got = plist(
            Path::new("/Users/a&b/.local/bin/wsp"),
            Path::new("/Users/a&b/<state>"),
            "/usr/bin",
        );
        assert!(got.contains("/Users/a&amp;b/.local/bin/wsp"), "{got}");
        assert!(!got.contains("/Users/a&b/"), "an unescaped ampersand reached the plist: {got}");
    }

    /// Idempotent, because an install is run again after every build and a file
    /// that changes when nothing did is one nobody can account for later. The
    /// comparison is the whole file, not a field — a plist somebody has edited
    /// by hand is exactly what this should replace.
    #[test]
    fn writing_it_twice_writes_it_once() {
        let dir = scratch("plan");
        let path = dir.join("com.wsp.daemon.plist");
        let want = plist(Path::new("/wsp"), &dir, "/usr/bin");

        assert_eq!(plan(&want, &path), Plan::Write, "nothing there and it said otherwise");
        std::fs::write(&path, &want).unwrap();
        assert_eq!(plan(&want, &path), Plan::Unchanged, "the same text was called a rewrite");

        // Somebody's hand edit is not "close enough".
        std::fs::write(&path, want.replace("RunAtLoad", "RunAtLoadX")).unwrap();
        assert_eq!(plan(&want, &path), Plan::Write, "a hand-edited plist was left alone");
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// `~/.local/bin` first, whatever the installer's PATH was. A launchd job
    /// that found a different `wsp` would be a loop of the wrong one, and
    /// `path_helper` does not put that directory on a login PATH at all, which
    /// is the whole reason it has to be written here.
    #[test]
    fn the_path_written_into_the_job_starts_at_our_own_bin() {
        let got = path_for(Some(OsStr::new("/usr/bin:/bin")));
        assert!(got.starts_with(&util::home().join(".local/bin").display().to_string()), "{got}");
        assert!(got.ends_with("/usr/bin:/bin"), "the installer's own PATH was dropped: {got}");

        // Directories that are not there are not written down: a PATH naming a
        // build tree that `git clean` took away is a job that cannot find
        // anything.
        let got = path_for(Some(OsStr::new("/usr/bin:/nope/nowhere:/bin")));
        assert!(!got.contains("nowhere"), "{got}");
    }

    /// A sandbox writes nothing and loads nothing. Its daemon would be pointed
    /// at `~/wsp` and the live backend's socket, and would then sync and reap
    /// against both — which is the leak `robustness-021` was filed for, and the
    /// one thing `WSP_HOME` makes easy to do by accident.
    ///
    /// Asked of [`sandboxed_in`] rather than of this process, because a test
    /// that exported `WSP_HOME` would be changing `crate::util::isolated` for
    /// every test that runs after it in the same process — a worse fault than
    /// the one it is checking for, and one that would make this test's result
    /// depend on which tests ran first.
    #[test]
    fn a_sandbox_is_refused_before_anything_is_touched() {
        let none: Option<&OsStr> = None;
        assert!(!sandboxed_in(none, none, none), "an ordinary install was read as a sandbox");
        assert!(sandboxed_in(Some(OsStr::new("/tmp/h")), none, none), "WSP_HOME alone");
        assert!(sandboxed_in(none, Some(OsStr::new("/tmp/s")), none), "WSP_STATE alone");
        assert!(sandboxed_in(none, none, Some(OsStr::new("/tmp/bin/wsp"))), "WSP_BIN alone");
    }

    /// The log path is under the state directory, and the state directory is
    /// the store's own — not a hardcoded `~/.local/state/wsp`, which would be
    /// the wrong place on a machine whose state has been pointed elsewhere and
    /// a directory nobody had asked for.
    #[test]
    fn the_log_goes_where_this_stores_its_machine_state() {
        let dir = scratch("log");
        let got = plist(Path::new("/wsp"), &dir, "/usr/bin");
        assert!(got.contains(&dir.join("daemon.log").display().to_string()), "{got}");
        let _ = std::fs::remove_dir_all(&dir);
    }
}