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

/// Run `systemctl --user ...`, turning a failure into its first stderr line.
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

fn active() -> bool {
    Command::new("systemctl")
        .args(["--user", "is-active", "--quiet", LABEL])
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
        let v = Command::new("systemctl").arg("--version").output().ok().map(|o| String::from_utf8_lossy(&o.stdout).into_owned());
        check_version(v.as_deref())?;
    }
    let step = decide(&change, active(), crate::launchd::sandboxed());
    let linger = linger();
    if step == Step::Nothing {
        return Ok(Done { path, wrote: false, loaded: false, linger });
    }
    if change == Plan::Write {
        crate::store::write_atomic(&path, &want)
            .map_err(|e| format!("cannot write {}: {e}", util::contract(&path)))?;
        ctl(&["daemon-reload"])?;
    }
    ctl(&["enable", "--now", LABEL])?;
    if step == Step::Restart {
        ctl(&["restart", LABEL])?;
    }
    settle(Duration::from_secs(8))?;
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
    format!(
        "{LABEL} is {}/{} (result {}, exec status {}, {} restarts) — `journalctl --user -u {LABEL}`",
        s.active,
        s.sub,
        if s.result.is_empty() { "unknown" } else { &s.result },
        s.exec_status.map(|c| c.to_string()).unwrap_or_else(|| "unknown".into()),
        s.restarts
    )
}

fn show() -> Show {
    Command::new("systemctl")
        .args(["--user", "show", "-p", "ActiveState,SubState,MainPID,NRestarts,Result,ExecMainStatus", LABEL])
        .output()
        .map(|o| show_from(&String::from_utf8_lossy(&o.stdout)))
        .unwrap_or_default()
}

fn settle(limit: Duration) -> Result<(), String> {
    let t = std::time::Instant::now();
    let mut held = None;
    loop {
        let s = show();
        if settled(&mut held, &s, t.elapsed().as_millis() as u64, 2000) {
            return Ok(());
        }
        if t.elapsed() >= limit {
            return Err(describe(&s));
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

/// Refuse a systemd too old for the unit this writes.
pub fn check_version(out: Option<&str>) -> Result<(), String> {
    match out.and_then(version_from) {
        Some(v) if v >= MIN_SYSTEMD => Ok(()),
        Some(v) => Err(format!("systemd {v} is too old: the unit needs Type=exec and StandardOutput=append: (systemd {MIN_SYSTEMD} or newer)")),
        None => Err("cannot read `systemctl --version`: is systemd running here?".into()),
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
    ctl(&["start", LABEL])?;
    settle(Duration::from_secs(8))?;
    Ok(format!("{LABEL} started"))
}

#[cfg(test)]
mod tests {
    use super::*;

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
        assert!(check_version(Some("systemd 255 (x)")).is_ok());
        assert!(check_version(Some("systemd 239 (x)")).unwrap_err().contains("too old"));
        assert!(check_version(None).is_err());
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
