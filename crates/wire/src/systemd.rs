//! The daemon's launcher on Linux: a systemd *user* unit that `wsp install`
//! writes and enables. The counterpart of [`crate::launchd`], with the same
//! contract: idempotent, a dry run says what it would write, a sandbox
//! (`WSP_HOME`/`WSP_STATE`/`WSP_BIN`, see [`crate::launchd::sandboxed`]) writes
//! and loads nothing.
//!
//! # Mapping from the launchd agent
//!
//! - `RunAtLoad` is `enable --now` with `WantedBy=default.target`.
//! - `KeepAlive { SuccessfulExit = false }` is `Restart=on-failure`: a daemon
//!   that was asked to go (SIGTERM, which it turns into exit 0) stays gone, and
//!   a crash or `kill -9` brings it back.
//! - `ThrottleInterval` is `RestartSec`.
//! - stdout and stderr are appended to `<state>/daemon.log`, the same file.
//!
//! # KillMode=process, and why
//!
//! systemd's default `KillMode=control-group` kills every process in the
//! unit's cgroup when the unit stops, restarts or exits, whatever `setsid` or
//! `process_group(0)` the process called: a cgroup is not a process group. The
//! daemon spawns long-lived agents (headless claude and its feeder,
//! `compound-sup` sessions, detached `wsp govern --ending` children), and under
//! the default every restart of the daemon would kill them all. launchd only
//! signals the job's own process (`AbandonProcessGroup` aside, its children
//! outlive it), so `KillMode=process` is what keeps the two the same: only the
//! daemon itself is signalled. A later improvement is to launch agents with
//! `systemd-run --user --scope`, so each lives in a cgroup of its own.
//!
//! # Type=exec
//!
//! `Type=simple` is "active" the moment systemd forks, before `exec`, so a
//! missing binary (status 203/EXEC) reads as started. `Type=exec` waits for the
//! exec and fails the start job if it fails. Both it and `StandardOutput=append:`
//! need systemd 240, which [`ensure`] checks.
//!
//! # Linger
//!
//! A user unit runs while the user has a session. On a VM nobody logs into,
//! that is "never" unless `loginctl enable-linger` was run, so the install
//! reports linger and says what to run when it is off.
//!
//! This module compiles on every platform (it is text generation plus
//! `systemctl` calls) so the unit-text and decision tests run on a Mac too.

use crate::launchd::Plan;
use crate::util;
use std::path::{Path, PathBuf};
use std::time::Duration;
use wsp_base::calls::Command;

/// The unit's name, without `.service`.
pub const LABEL: &str = "wsp-daemon";

/// Seconds systemd waits before restarting a daemon that died; launchd's
/// `ThrottleInterval`.
const RESTART_SEC: u32 = 30;

/// Where the unit goes: the user's own unit directory.
pub fn unit_path() -> PathBuf {
    util::home().join(".config/systemd/user").join(format!("{LABEL}.service"))
}

/// [`unit_path`] under the name both launchers share.
pub fn definition_path() -> PathBuf {
    unit_path()
}

/// The same as [`crate::launchd::definition`]: the launcher's file text.
pub fn definition(bin: &Path, state: &Path, path_var: &str) -> String {
    unit(bin, state, path_var)
}

/// Quote one word for a unit file's `ExecStart=`/`Environment=`: double quotes,
/// with `\`, `"` escaped and `%` (a specifier) and `$` (variable expansion)
/// doubled.
fn quote(s: &str) -> String {
    let mut out = String::from("\"");
    for c in s.chars() {
        match c {
            '\\' => out.push_str("\\\\"),
            '"' => out.push_str("\\\""),
            '%' => out.push_str("%%"),
            '$' => out.push_str("$$"),
            _ => out.push(c),
        }
    }
    out.push('"');
    out
}

/// Quote an `Environment=` assignment: `Environment=` does not expand `$`, so
/// unlike [`quote`] this leaves it alone; `\`, `"` and the `%` specifier are
/// escaped.
fn quote_env(s: &str) -> String {
    let mut out = String::from("\"");
    for c in s.chars() {
        match c {
            '\\' => out.push_str("\\\\"),
            '"' => out.push_str("\\\""),
            '%' => out.push_str("%%"),
            _ => out.push(c),
        }
    }
    out.push('"');
    out
}

/// A path as the value of `StandardOutput=append:`: unquoted, `%` doubled.
fn bare(s: &str) -> String {
    s.replace('%', "%%")
}

/// The unit `wsp install` writes, as text.
pub fn unit(bin: &Path, state: &Path, path_var: &str) -> String {
    let log = state.join("daemon.log").display().to_string();
    format!(
        "[Unit]\n\
         Description=wsp daemon\n\
         \n\
         [Service]\n\
         Type=exec\n\
         KillMode=process\n\
         ExecStart={bin} daemon\n\
         Environment={path}\n\
         Restart=on-failure\n\
         RestartSec={sec}\n\
         StandardOutput=append:{log}\n\
         StandardError=append:{log}\n\
         \n\
         [Install]\n\
         WantedBy=default.target\n",
        bin = quote(&bin.display().to_string()),
        path = quote_env(&format!("PATH={path_var}")),
        sec = RESTART_SEC,
        log = bare(&log),
    )
}

/// What the install has to do given the unit on disk and whether the service
/// is active now.
#[derive(Debug, PartialEq)]
pub enum Step {
    /// Right on disk and running: touch nothing, not even the daemon's pid.
    Nothing,
    /// Write if needed, `daemon-reload`, `enable --now`.
    Start,
    /// The unit changed under a running service: also `restart`, because a
    /// running service never reads the new file.
    Restart,
}

/// The decision, split from the `systemctl` calls so it is testable without a
/// systemd. `sandboxed` wins over everything: nothing is done.
pub fn decide(plan: &Plan, active: bool, sandboxed: bool) -> Step {
    match (sandboxed, plan, active) {
        (true, _, _) => Step::Nothing,
        (_, Plan::Unchanged, true) => Step::Nothing,
        (_, Plan::Write, true) => Step::Restart,
        (_, _, false) => Step::Start,
    }
}

/// [`crate::launchd::plan`], for the unit file.
pub fn plan(want: &str, path: &Path) -> Plan {
    crate::launchd::plan(want, path)
}

/// What `wsp install` did about the launcher.
#[derive(Debug, PartialEq)]
pub struct Done {
    pub path: PathBuf,
    pub wrote: bool,
    /// Whether the service was started or restarted now.
    pub loaded: bool,
    /// `loginctl`'s answer; `None` when it could not be asked.
    pub linger: Option<bool>,
}

impl Done {
    pub fn line(&self) -> String {
        let mut s = format!(
            "{} {} · {} — the daemon is a systemd user unit now, and it comes back on its own",
            util::contract(&self.path),
            if self.wrote { "written" } else { "unchanged" },
            match (self.loaded, self.wrote) {
                (true, _) => "enabled, and it is running now",
                (false, _) => "already running, and it picks this binary up on its own",
            }
        );
        if let Some(h) = linger_hint(self.linger) {
            s.push_str(&format!("\n  {h}"));
        }
        s
    }
}

/// The sentence for linger being off, `None` when it is on.
pub fn linger_hint(linger: Option<bool>) -> Option<String> {
    match linger {
        Some(true) => None,
        Some(false) => Some(
            "linger is off, so the daemon stops when the last login session ends and does not start at boot — run `loginctl enable-linger` once".into(),
        ),
        None => Some("linger could not be read (`loginctl show-user -p Linger`); if the daemon should outlive logins, run `loginctl enable-linger`".into()),
    }
}

/// Read `loginctl show-user -p Linger` output (`Linger=yes`).
pub fn linger_from(out: &str) -> Option<bool> {
    out.lines().find_map(|l| match l.trim().strip_prefix("Linger=")? {
        "yes" => Some(true),
        "no" => Some(false),
        _ => None,
    })
}

/// Is linger enabled for this user right now?
pub fn linger() -> Option<bool> {
    let user = Command::new("id").arg("-un").output().ok()?;
    let user = String::from_utf8_lossy(&user.stdout).trim().to_string();
    if user.is_empty() {
        return None;
    }
    let out = Command::new("loginctl").args(["show-user", &user, "-p", "Linger"]).output().ok()?;
    linger_from(&String::from_utf8_lossy(&out.stdout))
}

/// Run `systemctl --user ...`, turning a failure into its last non-empty stderr line.
fn ctl(args: &[&str]) -> Result<(), String> {
    let out = Command::new("systemctl")
        .arg("--user")
        .args(args)
        .output()
        .map_err(|e| format!("cannot run systemctl: {e}"))?;
    if out.status.success() {
        return Ok(());
    }
    let why = String::from_utf8_lossy(&out.stderr);
    let why = why.lines().rev().find(|l| !l.trim().is_empty()).unwrap_or("").trim();
    Err(format!(
        "systemctl --user {} failed: {}",
        args.first().copied().unwrap_or(""),
        if why.is_empty() { format!("exit {}", out.status.code().unwrap_or(-1)) } else { why.to_string() }
    ))
}

/// [`ctl`] for the calls that start the unit: a failure there carries what the
/// unit itself said — the last lines of `systemctl --user status` and its exec
/// status — since the stderr line of `enable --now` alone seldom says why the
/// daemon would not run.
fn ctl_start(args: &[&str]) -> Result<(), String> {
    ctl_start_unit(LABEL, args)
}

/// [`ctl_start`] for the unit `label`.
fn ctl_start_unit(label: &str, args: &[&str]) -> Result<(), String> {
    ctl(args).map_err(|e| format!("{e}{}", unit_said(label)))
}

fn unit_said(label: &str) -> String {
    let status = Command::new("systemctl")
        .args(["--user", "status", "--no-pager", "-n", "5", label])
        .output()
        .map(|o| String::from_utf8_lossy(&o.stdout).into_owned())
        .unwrap_or_default();
    said_of(label, &show_of(label), &status)
}

/// What to append to a start failure: the unit's state and exec status, and the
/// last few lines `status` printed. Empty when there is nothing to add.
pub fn said(show: &Show, status: &str) -> String {
    said_of(LABEL, show, status)
}

fn said_of(label: &str, show: &Show, status: &str) -> String {
    let mut out = String::new();
    if !show.active.is_empty() {
        out.push_str(&format!("\n  unit: {}", describe_unit(label, show)));
    }
    let tail: Vec<&str> = status.lines().filter(|l| !l.trim().is_empty()).collect();
    for l in &tail[tail.len().saturating_sub(5)..] {
        out.push_str(&format!("\n  | {}", l.trim_end()));
    }
    out
}

fn active(label: &str) -> bool {
    Command::new("systemctl")
        .args(["--user", "is-active", "--quiet", label])
        .status()
        .map(|s| s.success())
        .unwrap_or(false)
}

/// What `systemctl --user is-active` and `is-enabled` say, for `wsp doctor`
/// style reports: `(active, enabled)` as the words systemctl printed.
pub fn unit_state() -> (String, String) {
    let ask = |verb: &str| {
        Command::new("systemctl")
            .args(["--user", verb, LABEL])
            .output()
            .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string())
            .ok()
            .filter(|s| !s.is_empty())
            .unwrap_or_else(|| "unknown".into())
    };
    (ask("is-active"), ask("is-enabled"))
}

/// Write the unit if it is not already right, and make sure the service is
/// enabled and running; restart only when the unit text changed.
pub fn ensure(bin: &Path, state: &Path, path_var: &str) -> Result<Done, String> {
    let path = unit_path();
    let want = unit(bin, state, path_var);
    let change = plan(&want, &path);
    if !crate::launchd::sandboxed() {
        let v = Command::new("systemctl")
            .arg("--version")
            .output()
            .map(|o| String::from_utf8_lossy(&o.stdout).into_owned())
            .map_err(|e| e.to_string());
        check_version(v.as_deref().map_err(|e| e.as_str()))?;
    }
    let step = decide(&change, active(LABEL), crate::launchd::sandboxed());
    let linger = linger();
    if step == Step::Nothing {
        return Ok(Done { path, wrote: false, loaded: false, linger });
    }
    if change == Plan::Write {
        crate::store::write_atomic(&path, &want)
            .map_err(|e| format!("cannot write {}: {e}", util::contract(&path)))?;
        ctl(&["daemon-reload"])?;
    }
    ctl_start(&["enable", "--now", LABEL])?;
    if step == Step::Restart {
        ctl_start(&["restart", LABEL])?;
    }
    settle(LABEL, Duration::from_secs(8))?;
    Ok(Done { path, wrote: change == Plan::Write, loaded: true, linger })
}

/// What `systemctl --user show` said about the unit.
#[derive(Debug, Default, Clone, PartialEq)]
pub struct Show {
    pub active: String,
    pub sub: String,
    pub main_pid: u32,
    pub restarts: u32,
    pub result: String,
    pub exec_status: Option<i32>,
}

/// Read `show -p ActiveState,SubState,MainPID,NRestarts,Result,ExecMainStatus`.
pub fn show_from(out: &str) -> Show {
    let mut s = Show::default();
    for l in out.lines() {
        let Some((k, v)) = l.split_once('=') else { continue };
        let v = v.trim();
        match k {
            "ActiveState" => s.active = v.into(),
            "SubState" => s.sub = v.into(),
            "MainPID" => s.main_pid = v.parse().unwrap_or(0),
            "NRestarts" => s.restarts = v.parse().unwrap_or(0),
            "Result" => s.result = v.into(),
            "ExecMainStatus" => s.exec_status = v.parse().ok(),
            _ => {}
        }
    }
    s
}

/// What one sample means.
#[derive(Debug, PartialEq)]
pub enum Sample {
    /// Running as this pid after this many restarts.
    Running(u32, u32),
    /// Exited cleanly and not restarted: the daemon stood down on purpose
    /// (another holds the store), launchd's `Idle(Some(0))`.
    StoodDown,
    /// Anything else: starting, auto-restarting, failed.
    Other,
}

pub fn sample(s: &Show) -> Sample {
    match (s.active.as_str(), s.sub.as_str()) {
        ("active", "running") if s.main_pid != 0 => Sample::Running(s.main_pid, s.restarts),
        ("inactive", _) if s.result == "success" && s.exec_status == Some(0) => Sample::StoodDown,
        _ => Sample::Other,
    }
}

/// The settling rule, pure: `held` is the running `(pid, restarts)` seen and
/// since when (ms), `now` is ms. Returns whether the unit has settled. A pid or
/// restart-count change restarts the clock, so a crash loop never settles.
pub fn settled(held: &mut Option<(u32, u32, u64)>, s: &Show, now: u64, hold_ms: u64) -> bool {
    match sample(s) {
        Sample::StoodDown => true,
        Sample::Running(pid, n) => {
            match held {
                Some((p, r, _)) if *p == pid && *r == n => {}
                _ => *held = Some((pid, n, now)),
            }
            held.is_some_and(|(_, _, since)| now - since >= hold_ms)
        }
        Sample::Other => {
            *held = None;
            false
        }
    }
}

/// One sentence for a unit that did not settle.
pub fn describe(s: &Show) -> String {
    describe_unit(LABEL, s)
}

/// [`describe`] for the unit `label`.
fn describe_unit(label: &str, s: &Show) -> String {
    format!(
        "{label} is {}/{} (result {}, exec status {}, {} restarts) — `journalctl --user -u {label}`",
        s.active,
        s.sub,
        if s.result.is_empty() { "unknown" } else { &s.result },
        s.exec_status.map(|c| c.to_string()).unwrap_or_else(|| "unknown".into()),
        s.restarts
    )
}

fn show_of(label: &str) -> Show {
    Command::new("systemctl")
        .args(["--user", "show", "-p", "ActiveState,SubState,MainPID,NRestarts,Result,ExecMainStatus", label])
        .output()
        .map(|o| show_from(&String::from_utf8_lossy(&o.stdout)))
        .unwrap_or_default()
}

fn settle(label: &str, limit: Duration) -> Result<(), String> {
    let t = std::time::Instant::now();
    let mut held = None;
    loop {
        let s = show_of(label);
        if settled(&mut held, &s, t.elapsed().as_millis() as u64, 2000) {
            return Ok(());
        }
        if t.elapsed() >= limit {
            return Err(describe_unit(label, &s));
        }
        std::thread::sleep(Duration::from_millis(250));
    }
}

/// `systemctl --version`'s first line, `systemd 255 (255.4-1ubuntu8)`.
pub fn version_from(out: &str) -> Option<u32> {
    out.lines().next()?.split_whitespace().nth(1)?.parse().ok()
}

/// Oldest systemd with `Type=exec` and `StandardOutput=append:`.
const MIN_SYSTEMD: u32 = 240;

/// Refuse a systemd too old for the unit this writes. `Err` is why
/// `systemctl --version` could not be run at all (no such binary), which is a
/// different answer from one that ran and printed nothing readable.
pub fn check_version(out: Result<&str, &str>) -> Result<(), String> {
    let out = match out {
        Ok(o) => o,
        Err(e) => return Err(format!("cannot run `systemctl`: {e} — is systemd installed (a systemd user manager is needed)?")),
    };
    match version_from(out) {
        Some(v) if v >= MIN_SYSTEMD => Ok(()),
        Some(v) => Err(format!("systemd {v} is too old: the unit needs Type=exec and StandardOutput=append: (systemd {MIN_SYSTEMD} or newer)")),
        None => Err("`systemctl --version` printed no version wsp can read".into()),
    }
}

/// Put the daemon unit back: start it. The caller has established that no
/// daemon answers.
pub fn reload() -> Result<String, String> {
    let path = unit_path();
    if !path.exists() {
        return Err(format!("{} is not there — `wsp install --daemon` writes it", util::contract(&path)));
    }
    ctl(&["daemon-reload"])?;
    ctl(&["enable", LABEL])?;
    // `start`, like launchd's kickstart without -k: a running job is left alone.
    ctl_start(&["start", LABEL])?;
    settle(LABEL, Duration::from_secs(8))?;
    Ok(format!("{LABEL} started"))
}

// ---- the manager's unit (docs/agent-manager.md 2.4; wsp-024) -----------------

/// The manager's unit, without `.service`, beside the daemon's [`LABEL`].
pub const MANAGER_LABEL: &str = "wsp-manager";

/// What the manager is, in the line `wsp install` prints.
pub const MANAGER_KIND: &str = "a systemd user unit";

/// Where the manager's unit goes under `home`; a function of `home`, so a test
/// writes into a temp HOME.
pub fn manager_unit_path_in(home: &Path) -> PathBuf {
    home.join(".config/systemd/user").join(format!("{MANAGER_LABEL}.service"))
}

/// [`manager_unit_path_in`] for this user, under the name both launchers share.
pub fn manager_definition_path() -> PathBuf {
    manager_unit_path_in(&util::home())
}

/// The manager's unit as text: [`crate::launchd::manager_plist`] mapped as the
/// module header maps the daemon's, with two differences that plist has too.
///
/// - `Restart=always`, launchd's plain `KeepAlive true`: the manager has no
///   clean exit to leave alone, and one that declines to start (another answers
///   its port) is tried again every [`crate::launchd::MANAGER_THROTTLE`]
///   seconds until the other is gone.
/// - `KillMode=process` matters more here than for the daemon: the manager's
///   seats are its children's process groups, and a seat outlives its manager
///   (a restart adopts it) — under the default every manager restart would end
///   every seat on the machine.
///
/// `--port` is written out, so the machine record and the process cannot
/// disagree about it; the log is the plist's `<exec_dir>/log/launchd.log`.
pub fn manager_definition(bin: &Path, exec_dir: &Path, port: u16, path_var: &str) -> String {
    let log = crate::launchd::manager_log(exec_dir).display().to_string();
    format!(
        "[Unit]\n\
         Description=wsp agent manager\n\
         \n\
         [Service]\n\
         Type=exec\n\
         KillMode=process\n\
         ExecStart={bin} --exec-dir {exec_dir} --port {port}\n\
         Environment={path}\n\
         Restart=always\n\
         RestartSec={sec}\n\
         StandardOutput=append:{log}\n\
         StandardError=append:{log}\n\
         \n\
         [Install]\n\
         WantedBy=default.target\n",
        bin = quote(&bin.display().to_string()),
        exec_dir = quote(&exec_dir.display().to_string()),
        path = quote_env(&format!("PATH={path_var}")),
        sec = crate::launchd::MANAGER_THROTTLE,
        log = bare(&log),
    )
}

/// [`decide`] for the manager: a unit whose text did not change but whose binary
/// was just replaced (`restarted`) is restarted to run it, unless sandboxed.
pub fn manager_step(change: &Plan, active: bool, sandboxed: bool, restarted: bool) -> Step {
    match decide(change, active, sandboxed) {
        Step::Nothing if restarted && !sandboxed => Step::Restart,
        step => step,
    }
}

/// Write the manager's unit if it differs and make sure it is enabled and
/// running: [`ensure`]'s steps for [`MANAGER_LABEL`]. `restarted` says the
/// binary beside it was just replaced, so a running unit whose text did not
/// change is restarted to run it.
pub fn ensure_manager(bin: &Path, exec_dir: &Path, port: u16, path_var: &str, restarted: bool) -> Result<Done, String> {
    let _ = std::fs::create_dir_all(exec_dir.join("log"));
    let path = manager_definition_path();
    let want = manager_definition(bin, exec_dir, port, path_var);
    let change = plan(&want, &path);
    let sandboxed = crate::launchd::sandboxed();
    if !sandboxed {
        let v = Command::new("systemctl")
            .arg("--version")
            .output()
            .map(|o| String::from_utf8_lossy(&o.stdout).into_owned())
            .map_err(|e| e.to_string());
        check_version(v.as_deref().map_err(|e| e.as_str()))?;
    }
    let linger = linger();
    let step = manager_step(&change, active(MANAGER_LABEL), sandboxed, restarted);
    if step == Step::Nothing {
        return Ok(Done { path, wrote: false, loaded: false, linger });
    }
    if change == Plan::Write {
        crate::store::write_atomic(&path, &want).map_err(|e| format!("cannot write {}: {e}", util::contract(&path)))?;
        ctl(&["daemon-reload"])?;
    }
    ctl_start_unit(MANAGER_LABEL, &["enable", "--now", MANAGER_LABEL])?;
    if step == Step::Restart {
        ctl_start_unit(MANAGER_LABEL, &["restart", MANAGER_LABEL])?;
    }
    settle(MANAGER_LABEL, Duration::from_secs(8))?;
    Ok(Done { path, wrote: change == Plan::Write, loaded: true, linger })
}

/// Stop and disable the manager's unit and remove its file, as
/// [`crate::launchd::remove_manager`] boots its job out. Nothing to stop and
/// nothing to remove is success; the value says whether there was a unit file.
pub fn remove_manager() -> Result<bool, String> {
    let path = manager_definition_path();
    remove_unit(path.exists(), || active(MANAGER_LABEL), ctl, || std::fs::remove_file(&path), &util::contract(&path))
}

/// [`remove_manager`]'s decisions, with what it does to the machine passed in
/// (`active` asks the unit, `ctl` runs `systemctl --user`, `remove_file` drops
/// the unit file), so they are tested without a systemd. `had` says the unit
/// file existed.
///
/// - disable (and stop) when the file existed or the unit is active;
/// - fail when the unit is still active afterwards;
/// - a file already gone is fine, any other failure to remove it is not;
/// - `daemon-reload` only when there was a file to forget.
fn remove_unit(
    had: bool,
    mut active: impl FnMut() -> bool,
    mut ctl: impl FnMut(&[&str]) -> Result<(), String>,
    remove_file: impl FnOnce() -> std::io::Result<()>,
    shown: &str,
) -> Result<bool, String> {
    if had || active() {
        ctl(&["disable", "--now", MANAGER_LABEL])?;
    }
    if active() {
        return Err(format!("{MANAGER_LABEL} is still active after `systemctl --user disable --now`"));
    }
    match remove_file() {
        Ok(()) => {}
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
        Err(e) => return Err(format!("cannot remove {shown}: {e}")),
    }
    if had {
        ctl(&["daemon-reload"])?;
    }
    Ok(had)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn removing_the_manager_disables_what_is_there_and_reloads_only_for_a_file() {
        use std::cell::{Cell, RefCell};
        use std::io::{Error, ErrorKind};
        let disable = "disable --now wsp-manager";
        // A machine: whether the unit runs (`disable --now` stops it unless
        // `stuck`), and what `systemctl` was asked.
        struct Machine {
            running: Cell<bool>,
            stuck: bool,
            calls: RefCell<Vec<String>>,
        }
        impl Machine {
            fn new(running: bool, stuck: bool) -> Machine {
                Machine { running: Cell::new(running), stuck, calls: RefCell::new(Vec::new()) }
            }
            fn ctl(&self, a: &[&str]) -> Result<(), String> {
                self.calls.borrow_mut().push(a.join(" "));
                if a[0] == "disable" && !self.stuck {
                    self.running.set(false);
                }
                Ok(())
            }
            fn remove(&self, had: bool, rm: impl FnOnce() -> std::io::Result<()>) -> Result<bool, String> {
                remove_unit(had, || self.running.get(), |a| self.ctl(a), rm, "~/u.service")
            }
            fn said(&self) -> Vec<String> {
                self.calls.borrow().clone()
            }
        }
        let ok = || Ok(());
        let gone = || Err(Error::from(ErrorKind::NotFound));

        // A file and a running unit: disabled, file removed, reloaded, reported.
        let m = Machine::new(true, false);
        assert_eq!(m.remove(true, ok), Ok(true));
        assert_eq!(m.said(), [disable, "daemon-reload"]);

        // A file whose unit was never active: still disabled, and reloaded.
        let m = Machine::new(false, false);
        assert_eq!(m.remove(true, ok), Ok(true));
        assert_eq!(m.said(), [disable, "daemon-reload"]);

        // Nothing there at all: nothing run, and not a failure.
        let m = Machine::new(false, false);
        assert_eq!(m.remove(false, gone), Ok(false));
        assert!(m.said().is_empty(), "{:?}", m.said());

        // A running unit with no file: disabled, but no reload for a file never there.
        let m = Machine::new(true, false);
        assert_eq!(m.remove(false, gone), Ok(false));
        assert_eq!(m.said(), [disable]);

        // Still active afterwards: an error, and the file is left alone.
        let m = Machine::new(true, true);
        let removed = Cell::new(false);
        let err = m
            .remove(true, || {
                removed.set(true);
                Ok(())
            })
            .unwrap_err();
        assert!(err.contains("still active"), "{err}");
        assert!(!removed.get());
        assert_eq!(m.said(), [disable]);

        // A failure to remove the file other than "already gone" is heard.
        let m = Machine::new(false, false);
        let err = m.remove(true, || Err(Error::from(ErrorKind::PermissionDenied))).unwrap_err();
        assert!(err.starts_with("cannot remove ~/u.service"), "{err}");
        assert_eq!(m.said(), [disable], "no reload after a failed removal");
    }

    #[test]
    fn a_replaced_manager_binary_restarts_a_unit_whose_text_did_not_change() {
        use Plan::{Unchanged, Write};
        assert_eq!(manager_step(&Unchanged, true, false, true), Step::Restart, "the new binary must run");
        assert_eq!(manager_step(&Unchanged, true, false, false), Step::Nothing);
        assert_eq!(manager_step(&Unchanged, true, true, true), Step::Nothing, "a sandbox does nothing");
        assert_eq!(manager_step(&Write, true, false, false), Step::Restart);
        assert_eq!(manager_step(&Unchanged, false, false, true), Step::Start, "a stopped unit starts, not restarts");
    }

    #[test]
    fn the_managers_unit_names_what_2_4_asks() {
        let home = Path::new("/home/u");
        assert!(manager_unit_path_in(home).ends_with(".config/systemd/user/wsp-manager.service"));
        let u = manager_definition(
            Path::new("/home/u/.local/bin/wsp-manager"),
            Path::new("/home/u/.local/state/wsp-manager"),
            7487,
            "/usr/bin:/bin",
        );
        assert!(
            u.contains("ExecStart=\"/home/u/.local/bin/wsp-manager\" --exec-dir \"/home/u/.local/state/wsp-manager\" --port 7487\n"),
            "{u}"
        );
        assert!(u.contains("Restart=always\n"), "no clean exit to leave alone: {u}");
        assert!(u.contains("KillMode=process\n"), "a seat outlives its manager: {u}");
        assert!(u.contains("Type=exec\n"));
        assert!(u.contains(&format!("RestartSec={}\n", crate::launchd::MANAGER_THROTTLE)));
        assert!(u.contains("StandardOutput=append:/home/u/.local/state/wsp-manager/log/launchd.log\n"), "{u}");
        assert!(u.contains("Environment=\"PATH=/usr/bin:/bin\"\n"));
        assert!(u.contains("WantedBy=default.target"));
    }

    #[test]
    fn unit_text_has_what_systemd_needs() {
        let u = unit(Path::new("/home/u/.local/bin/wsp"), Path::new("/home/u/.local/state/wsp"), "/home/u/.local/bin:/usr/bin");
        assert!(u.contains("ExecStart=\"/home/u/.local/bin/wsp\" daemon\n"), "{u}");
        assert!(u.contains("Environment=\"PATH=/home/u/.local/bin:/usr/bin\"\n"));
        assert!(u.contains("Restart=on-failure\n"));
        assert!(u.contains("StandardOutput=append:/home/u/.local/state/wsp/daemon.log\n"));
        assert!(u.contains("StandardError=append:/home/u/.local/state/wsp/daemon.log\n"));
        assert!(u.contains("WantedBy=default.target"));
    }

    #[test]
    fn unit_keeps_agents_alive_and_waits_for_exec() {
        let u = unit(Path::new("/b/wsp"), Path::new("/s"), "/p");
        assert!(u.contains("KillMode=process\n"), "{u}");
        assert!(u.contains("Type=exec\n"));
        assert!(u.contains("Restart=on-failure\n"));
    }

    #[test]
    fn environment_keeps_dollar_but_escapes_percent_and_quotes() {
        let u = unit(Path::new("/b/wsp"), Path::new("/s"), "/a$b:/c%d\"e");
        assert!(u.contains("Environment=\"PATH=/a$b:/c%%d\\\"e\"\n"), "{u}");
    }

    #[test]
    fn show_is_judged() {
        let run = show_from("ActiveState=active\nSubState=running\nMainPID=42\nNRestarts=0\nResult=success\nExecMainStatus=0\n");
        assert_eq!(sample(&run), Sample::Running(42, 0));
        let exec_fail = show_from("ActiveState=activating\nSubState=auto-restart\nMainPID=0\nNRestarts=3\nResult=exit-code\nExecMainStatus=203\n");
        assert_eq!(sample(&exec_fail), Sample::Other);
        assert!(describe(&exec_fail).contains("203"));
        let down = show_from("ActiveState=inactive\nSubState=dead\nMainPID=0\nNRestarts=0\nResult=success\nExecMainStatus=0\n");
        assert_eq!(sample(&down), Sample::StoodDown);
        let failed = show_from("ActiveState=failed\nSubState=failed\nMainPID=0\nResult=exit-code\nExecMainStatus=1\n");
        assert_eq!(sample(&failed), Sample::Other);
    }

    #[test]
    fn settling_needs_the_same_pid_and_restarts_for_the_hold() {
        let at = |pid, n| Show { active: "active".into(), sub: "running".into(), main_pid: pid, restarts: n, result: "success".into(), exec_status: Some(0) };
        let mut h = None;
        assert!(!settled(&mut h, &at(5, 0), 0, 2000));
        assert!(!settled(&mut h, &at(5, 0), 1500, 2000));
        assert!(settled(&mut h, &at(5, 0), 2000, 2000));
        let mut h = None;
        assert!(!settled(&mut h, &at(5, 0), 0, 2000));
        assert!(!settled(&mut h, &at(6, 1), 1900, 2000), "a restart resets the clock");
        assert!(!settled(&mut h, &at(6, 1), 3000, 2000));
        assert!(settled(&mut h, &at(6, 1), 3900, 2000));
        let mut h = None;
        assert!(!settled(&mut h, &at(5, 0), 0, 2000));
        assert!(!settled(&mut h, &Show::default(), 1000, 2000));
        assert!(!settled(&mut h, &at(5, 0), 2500, 2000), "a gap in running resets it");
    }

    #[test]
    fn version_gate() {
        assert_eq!(version_from("systemd 255 (255.4-1ubuntu8)\n+PAM"), Some(255));
        assert!(check_version(Ok("systemd 255 (x)")).is_ok());
        assert!(check_version(Ok("systemd 239 (x)")).unwrap_err().contains("too old"));
        let unreadable = check_version(Ok("")).unwrap_err();
        assert!(unreadable.contains("printed no version") && !unreadable.contains("running"), "{unreadable}");
        let missing = check_version(Err("No such file or directory (os error 2)")).unwrap_err();
        assert!(missing.contains("cannot run `systemctl`") && missing.contains("installed"), "{missing}");
    }

    #[test]
    fn a_failed_start_says_what_the_unit_said() {
        let s = show_from("ActiveState=activating\nSubState=auto-restart\nMainPID=0\nNRestarts=3\nResult=exit-code\nExecMainStatus=203\n");
        let m = said(&s, "x unit\n   Active: activating\n\nNov 17 wsp[1]: boom\n");
        assert!(m.contains("exec status 203") && m.contains("boom"), "{m}");
        assert_eq!(said(&Show::default(), ""), "");
    }

    #[test]
    fn unit_text_escapes_specifiers_and_variables() {
        let u = unit(Path::new("/h/a b/100%/$x/wsp"), Path::new("/s"), "/p");
        assert!(u.contains("ExecStart=\"/h/a b/100%%/$$x/wsp\" daemon"), "{u}");
    }

    #[test]
    fn unit_is_deterministic() {
        let a = unit(Path::new("/b/wsp"), Path::new("/s"), "/p");
        assert_eq!(a, unit(Path::new("/b/wsp"), Path::new("/s"), "/p"));
    }

    #[test]
    fn plan_is_unchanged_only_for_identical_text() {
        let dir = std::env::temp_dir().join(format!("wsp-systemd-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let f = dir.join("wsp-daemon.service");
        let _ = std::fs::remove_file(&f);
        assert_eq!(plan("a", &f), Plan::Write);
        std::fs::write(&f, "a").unwrap();
        assert_eq!(plan("a", &f), Plan::Unchanged);
        assert_eq!(plan("b", &f), Plan::Write);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn decision_table() {
        assert_eq!(decide(&Plan::Unchanged, true, false), Step::Nothing);
        assert_eq!(decide(&Plan::Unchanged, false, false), Step::Start);
        assert_eq!(decide(&Plan::Write, true, false), Step::Restart);
        assert_eq!(decide(&Plan::Write, false, false), Step::Start);
        for p in [Plan::Write, Plan::Unchanged] {
            for a in [true, false] {
                assert_eq!(decide(&p, a, true), Step::Nothing, "a sandbox does nothing");
            }
        }
    }

    #[test]
    fn linger_is_read_from_loginctl() {
        assert_eq!(linger_from("Linger=yes\n"), Some(true));
        assert_eq!(linger_from("Linger=no\n"), Some(false));
        assert_eq!(linger_from(""), None);
        assert!(linger_hint(Some(false)).unwrap().contains("loginctl enable-linger"));
        assert!(linger_hint(Some(true)).is_none());
    }

    #[test]
    fn done_line_mentions_linger_only_when_off() {
        let mut d = Done { path: PathBuf::from("/x/wsp-daemon.service"), wrote: true, loaded: true, linger: Some(true) };
        assert!(!d.line().contains("linger"));
        d.linger = Some(false);
        assert!(d.line().contains("enable-linger"));
    }
}
