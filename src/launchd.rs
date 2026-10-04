//! The daemon's launcher: a launchd user agent that `wsp install` writes and loads.
//!
//! # What this replaces, and what it was costing
//!
//! `wsp daemon` was started by herdr's `[[startup]]` — one line in
//! `herdr-plugin/herdr-plugin.toml`, and the whole of how a machine got a
//! daemon. herdr is gone on this machine, so the daemon is gone with it:
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
//! `executor/herdr-server.plist` argues the same thing for the executor's
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
//! herdr's `[[startup]]` is not being removed here, only demoted: a machine
//! where `wsp install` has never run still gets a daemon when herdr starts, and
//! that is the safety net for the machine this lands on. So launchd's job and
//! herdr's can both be pointed at the same store, and `crate::daemon`'s one-
//! daemon-per-store rule is what makes that safe rather than a second copy of
//! t-260817-052. Nothing here coordinates with it, and nothing here should: the
//! refusal names the pid and `launchd` stays down, which is the outcome the
//! `KeepAlive` policy below is chosen for.
//!
//! # Why `KeepAlive` is not `true`
//!
//! Because `wsp daemon` exits **0** when it declines to run — a second daemon
//! on one store is not a failure, and herdr's startup hook used to rely on
//! that. A plain `<true/>` would restart it immediately, it would decline
//! again, and launchd would spend the rest of the day running `wsp daemon` in a
//! loop. `SuccessfulExit: false` restarts a crash and a `kill -9` and leaves a
//! clean exit alone, and `ThrottleInterval` is here for the case the policy
//! still admits: a build that segfaults on startup, restarted every ten seconds
//! for ever, which is a machine nobody can install anything on.

use std::ffi::OsStr;
use std::path::{Path, PathBuf};
use std::process::Command;

use crate::util;

/// The job's name in launchd and in `~/Library/LaunchAgents`.
///
/// Not `edjames.wsp`, which is the herdr plugin's id: this is a different
/// mechanism on a machine that may have several logins, and a launchd label is
/// what `launchctl print` and `bootout` take, so it has to be the string in the
/// plist and nowhere else.
pub(crate) const LABEL: &str = "com.wsp.daemon";

/// How long launchd waits before restarting a job that keeps dying. Without it
/// the default is ten seconds, which is fast enough to be noise in a log nobody
/// reads and slow enough to look like a daemon that keeps trying.
const THROTTLE: u32 = 30;

/// Where the plist goes. A *user* agent: a daemon on somebody's machine,
/// started at their login, and not something to be installed system-wide.
pub(crate) fn plist_path() -> PathBuf {
    util::home().join("Library/LaunchAgents").join(format!("{LABEL}.plist"))
}

/// Is this instance a sandbox, where a launchd job would reach out of it?
///
/// The one place that decides, and it is the same test `cmd_install::install`
/// makes before it will replace a binary a sandbox does not own. Both exist
/// because `WSP_HOME` and `WSP_STATE` make a complete wsp instance and nothing
/// else: the daemon this writes would be pointed at `~/wsp` and at a socket
/// belonging to the live herdr, and would then sync and reap against it, which
/// is the leak `robustness-021` was filed for. A throwaway `WSP_HOME` is the
/// most common way that happens.
pub(crate) fn sandboxed() -> bool {
    sandboxed_in(
        std::env::var_os("WSP_HOME").as_deref(),
        std::env::var_os("WSP_STATE").as_deref(),
        std::env::var_os("WSP_BIN").as_deref(),
    )
}

/// The same rule over values handed in, so it can be asked without the test
/// mutating this process's environment. `WSP_HOME` and `WSP_STATE` are the two
/// that make a complete wsp instance; `WSP_BIN` is what `herdr-plugin/run.sh`
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
pub(crate) fn path_for(installer: Option<&OsStr>) -> String {
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
pub(crate) fn plist(bin: &Path, state: &Path, path_var: &str) -> String {
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
pub(crate) enum Plan {
    /// Nothing there, or what is there is not what this machine wants.
    Write,
    /// Byte for byte what is already there. Nothing is written and nothing is
    /// said about having written it.
    Unchanged,
}

pub(crate) fn plan(want: &str, path: &Path) -> Plan {
    match std::fs::read_to_string(path) {
        Ok(have) if have == want => Plan::Unchanged,
        _ => Plan::Write,
    }
}

/// The `gui/$UID` domain, which is where a user agent belongs.
///
/// `gui` rather than `system` for the reason `executor/herdr-server.plist`
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
pub(crate) struct Done {
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
    pub(crate) fn line(&self) -> String {
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
/// Failure here is **not** an install failure. The binary is on disk and every
/// pane has re-exec'd into it whatever launchd does, so this returns its error
/// to be printed under a `✓ install` rather than turning a good install red.
pub(crate) fn ensure(bin: &Path, state: &Path, path_var: &str) -> Result<Done, String> {
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
    let want = plist_as(label, bin, state, path_var);
    let change = plan(&want, path);
    let domain = domain()?;
    let target = format!("{domain}/{label}");

    // Already right and already there: nothing. Not an optimisation, and not
    // politeness — the verb that *could* be used here is measurably wrong.
    // `launchctl kickstart -k` on a job that is running waits out
    // `ThrottleInterval` first, measured on this machine at 28 seconds of an
    // install standing still, and 57 on a job that has stopped; `bootout` plus
    // `bootstrap` below is instant in both. And there is nothing to restart
    // for: the daemon re-`exec`s the file that was just installed within a
    // tick, which is the whole reason the plist points at `~/.local/bin/wsp`
    // rather than at a build tree.
    if change == Plan::Unchanged && loaded(&target) {
        return Ok(Done { path: path.to_path_buf(), wrote: false, loaded: false });
    }

    if change == Plan::Write {
        crate::store::write_atomic(path, &want)
            .map_err(|e| format!("cannot write {}: {e}", util::contract(path)))?;
    }
    // `bootout` before `bootstrap`, always, and both because the plist changed
    // and because `bootstrap` on a loaded label fails with `Input/output error`
    // and a suggestion to re-run as root. The pair is also the only way to get
    // the job onto the settings just written — a `bootstrap` that failed is not
    // going to read the new file — and it is instant where `kickstart` is not:
    // bootout and bootstrap are a fresh load, with none of the throttle history
    // a restart inherits. `bootout` failing is the normal case of a job that was
    // not loaded at all, so its complaint is dropped and `bootstrap`'s is not.
    let _ = Command::new("launchctl")
        .arg("bootout")
        .arg(&target)
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status();
    bootstrap(&domain, path)?;
    Ok(Done { path: path.to_path_buf(), wrote: change == Plan::Write, loaded: true })
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
    use std::fs::Permissions;
    use std::os::unix::fs::PermissionsExt as _;
    use std::time::Duration;

    fn scratch(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("wsp-launchd-{}-{}", name, std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
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
        let dir = scratch("agent");
        let label = format!("com.wsp.test-{}", std::process::id());
        let path = dir.join(format!("{label}.plist"));

        // Stands in for the daemon: a program that records what it was given and
        // exits cleanly — which is also the case the `KeepAlive` policy is
        // chosen for.
        let ran = dir.join("ran.log");
        let bin = dir.join("fake-wsp");
        std::fs::write(
            &bin,
            format!("#!/bin/sh\nprintf '%s\\n' \"$*\" >> {}\nexit 0\n", ran.display()),
        )
        .unwrap();
        std::fs::set_permissions(&bin, Permissions::from_mode(0o755)).unwrap();

        let first = ensure_as(&label, &path, &bin, &dir, "/usr/bin:/bin").expect("the first install");
        assert!(first.wrote, "nothing was written on the first run");
        assert!(first.loaded, "a job that was not loaded was not bootstrapped");
        assert!(loaded(&format!("{}/{label}", domain().unwrap())), "the job did not survive being written");

        // `RunAtLoad`, and the `daemon` argument, actually reaching the program.
        for _ in 0..100 {
            if ran.exists() { break }
            std::thread::sleep(Duration::from_millis(50));
        }
        assert!(
            std::fs::read_to_string(&ran).unwrap_or_default().contains("daemon"),
            "the job ran something other than `wsp daemon`"
        );

        // The ordinary install: same text, same job. Nothing, because
        // `kickstart -k` on a running job waits out `ThrottleInterval` first —
        // measured at 28 seconds of an install standing still — and there is no
        // restart to do, the daemon re-execs what was just installed by itself.
        let second = ensure_as(&label, &path, &bin, &dir, "/usr/bin:/bin").expect("the second install");
        assert!(!second.wrote, "an identical plist was rewritten");
        assert!(!second.loaded, "a job that was already loaded was booted out for nothing");
        assert!(loaded(&format!("{}/{label}", domain().unwrap())), "and the job did not survive that either");

        // A plist that *did* change is reloaded, because `bootstrap` on a
        // loaded label fails outright and nothing else would ever read the new
        // file.
        std::fs::write(&path, "<!-- edited by hand -->").unwrap();
        let third = ensure_as(&label, &path, &bin, &dir, "/usr/bin:/bin").expect("the third install");
        assert!(third.wrote, "a hand-edited plist was left alone");
        assert!(third.loaded, "and the job was never told about it");

        let out = Command::new("launchctl")
            .args(["bootout", &format!("{}/{}", domain().unwrap(), label)])
            .output()
            .unwrap();
        assert!(out.status.success(), "bootout failed: {}", String::from_utf8_lossy(&out.stderr));
        assert!(!loaded(&format!("{}/{label}", domain().unwrap())), "the job outlived its bootout");
        let _ = std::fs::remove_dir_all(&dir);
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
    /// at `~/wsp` and the live herdr's socket, and would then sync and reap
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