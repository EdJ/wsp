//! A repository that has stopped packing (`wsp-380`).
//!
//! A detached `git gc` that crashes leaves `gc.log.lock` (and often `gc.pid`)
//! behind, and from then on every auto-gc finds the lock, gives up, and the
//! repository never packs again: 22,770 loose objects in the store, `wsp ls`
//! at 1.35 s instead of 0.28 s. macOS makes it likely, because a forked git
//! that touches CoreFoundation dies with "The process has forked…", and that
//! text is what the lock then holds.
//!
//! `wsp doctor` looks at the store and every declared project root. It names a
//! lock whose owner is dead and a repository with more loose objects than
//! git's own auto threshold, and with `--fix` it clears the stale files, turns
//! `gc.autoDetach` off, and runs a niced `git gc`.
//!
//! What it will not do: touch a repository whose `gc.pid` names a live
//! process, run gc while a commit is in flight there (an `index.lock` in the
//! git dir or any of its worktrees), or prune anything younger than git's own
//! two-week grace — plain `git gc`, never `prune --expire=now`.

use std::path::{Path, PathBuf};
use crate::host::Host;
use std::process::Stdio;
use wsp_base::calls::Command;
use std::time::Duration;

/// git's own `gc.auto` default: past this many loose objects it wants to pack.
pub const LOOSE: usize = 6700;

/// A lock with no pid in it is only called stale once it has sat this long.
const AGE: Duration = Duration::from_secs(10 * 60);

#[derive(Debug, Default, PartialEq)]
pub struct Finding {
    /// Lock and pid files whose owner is gone.
    pub stale: Vec<PathBuf>,
    /// A gc that is really running there: the repository is left entirely alone.
    pub live: bool,
    pub loose: usize,
    /// A commit is in flight in the git dir or one of its worktrees.
    pub busy: bool,
}

impl Finding {
    pub fn wants_attention(&self) -> bool {
        !self.live && (!self.stale.is_empty() || self.loose > LOOSE)
    }
}

fn pid_of(file: &Path) -> Option<u32> {
    std::fs::read_to_string(file).ok()?.split_whitespace().next()?.parse().ok()
}

pub fn alive(pid: u32) -> bool {
    // Could not ask reads as not alive, as it did before the Host.
    crate::host::LOCAL.probe(pid).unwrap_or(false)
}

fn loose_in(git: &Path) -> usize {
    let Ok(dirs) = std::fs::read_dir(git.join("objects")) else { return 0 };
    dirs.flatten()
        .filter(|d| {
            let n = d.file_name();
            let n = n.to_string_lossy();
            n.len() == 2 && n.bytes().all(|b| b.is_ascii_hexdigit())
        })
        .map(|d| std::fs::read_dir(d.path()).map(|r| r.flatten().count()).unwrap_or(0))
        .sum()
}

fn committing(git: &Path) -> bool {
    if git.join("index.lock").exists() {
        return true;
    }
    std::fs::read_dir(git.join("worktrees"))
        .map(|r| r.flatten().any(|w| w.path().join("index.lock").exists()))
        .unwrap_or(false)
}

/// What is wrong with the repository whose git dir is `git`. `alive` answers
/// whether a pid is a running process; `age` is how old a pid-less lock must be.
pub fn inspect(git: &Path, alive: &dyn Fn(u32) -> bool, age: Duration) -> Finding {
    let mut f = Finding { loose: loose_in(git), busy: committing(git), ..Finding::default() };
    let (pid, lock) = (git.join("gc.pid"), git.join("gc.log.lock"));
    if pid.exists() {
        match pid_of(&pid) {
            Some(p) if alive(p) => f.live = true,
            _ => f.stale.push(pid.clone()),
        }
    }
    if lock.exists() && !f.live {
        let old = std::fs::metadata(&lock)
            .and_then(|m| m.modified())
            .ok()
            .and_then(|t| t.elapsed().ok())
            .is_some_and(|e| e >= age);
        // The lock carries no pid of its own. A gc.pid that was just found
        // dead settles it; otherwise only its age can.
        if old || f.stale.contains(&pid) {
            f.stale.push(lock);
        }
    }
    f
}

/// The git dirs worth looking at: the store and every declared root that is a
/// repository of its own, once each.
pub fn repos(roots: &[PathBuf]) -> Vec<PathBuf> {
    let mut out: Vec<PathBuf> = Vec::new();
    for r in roots {
        let g = r.join(".git");
        if g.is_dir() && !out.contains(&g) {
            out.push(g);
        }
    }
    out
}

/// Clear the stale files, stop the detach that crashes, and pack. Returns what
/// was done, in words.
pub fn fix(git: &Path, f: &Finding, threshold: usize) -> Vec<String> {
    let mut done = Vec::new();
    if f.live || !f.wants_attention_at(threshold) {
        return done;
    }
    for s in &f.stale {
        if std::fs::remove_file(s).is_ok() {
            done.push(format!("removed {}", s.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default()));
        }
    }
    let set = Command::new("git")
        .arg("--git-dir")
        .arg(git)
        .args(["config", "gc.autoDetach", "false"])
        .status()
        .is_ok_and(|s| s.success());
    if set {
        done.push("set gc.autoDetach=false".into());
    }
    match (f.loose > threshold, f.busy) {
        (true, true) => done.push("not packing: a commit is in flight there, run `wsp doctor --fix` again".into()),
        (true, false) => {
            let ok = Command::new("nice")
                .args(["git", "--git-dir"])
                .arg(git)
                .args(["gc", "--quiet"])
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .status()
                .is_ok_and(|s| s.success());
            done.push(if ok { "ran git gc".into() } else { "git gc failed".into() });
        }
        _ => {}
    }
    done
}

impl Finding {
    fn wants_attention_at(&self, threshold: usize) -> bool {
        !self.live && (!self.stale.is_empty() || self.loose > threshold)
    }
}

fn describe(root: &Path, f: &Finding) -> String {
    let mut bits = Vec::new();
    if !f.stale.is_empty() {
        let names: Vec<String> =
            f.stale.iter().filter_map(|p| p.file_name()).map(|n| n.to_string_lossy().into_owned()).collect();
        bits.push(format!("a crashed gc left {} behind, so it can never pack", names.join(" and ")));
    }
    if f.loose > LOOSE {
        bits.push(format!("{} loose objects", f.loose));
    }
    format!("{}: {} — `wsp doctor --fix`", crate::util::contract(root), bits.join("; "))
}

/// The doctor's pass over `roots`. A stale lock is a problem (the repository
/// has stopped packing); loose objects alone are a note.
pub fn health(roots: &[PathBuf], fixing: bool, problems: &mut Vec<String>, notes: &mut Vec<String>) {
    health_with(roots, fixing, &alive, problems, notes)
}

fn health_with(
    roots: &[PathBuf],
    fixing: bool,
    is_alive: &dyn Fn(u32) -> bool,
    problems: &mut Vec<String>,
    notes: &mut Vec<String>,
) {
    for git in repos(roots) {
        let root = git.parent().unwrap_or(&git).to_path_buf();
        let f = inspect(&git, is_alive, AGE);
        if !f.wants_attention() {
            continue;
        }
        if fixing {
            for d in fix(&git, &f, LOOSE) {
                notes.push(format!("{}: {d}", crate::util::contract(&root)));
            }
            continue;
        }
        match f.stale.is_empty() {
            true => notes.push(describe(&root, &f)),
            false => problems.push(describe(&root, &f)),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn repo(name: &str) -> PathBuf {
        let base = std::env::var("WSP_SCRATCH").map(PathBuf::from).unwrap_or_else(|_| std::env::temp_dir());
        let d = base.join(format!("gc-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        let git = |a: &[&str]| {
            assert!(Command::new("git")
                .current_dir(&d)
                .args(["-c", "user.name=t", "-c", "user.email=t@t"])
                .args(a)
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .status()
                .unwrap()
                .success())
        };
        git(&["init", "-q"]);
        std::fs::write(d.join("f"), "x").unwrap();
        git(&["add", "f"]);
        git(&["commit", "-q", "-m", "one"]);
        d
    }

    fn plant(git: &Path) {
        std::fs::write(git.join("gc.pid"), "999999 somehost").unwrap();
        std::fs::write(git.join("gc.log.lock"), "The process has forked and you cannot use this CoreFoundation").unwrap();
    }

    #[test]
    fn a_stale_lock_and_dead_pid_are_reported_and_fixed() {
        let d = repo("stale");
        let git = d.join(".git");
        plant(&git);

        let (mut problems, mut notes) = (Vec::new(), Vec::new());
        health_with(&[d.clone()], false, &|_| false, &mut problems, &mut notes);
        assert_eq!(problems.len(), 1, "{problems:?}");
        assert!(problems[0].contains("gc.pid") && problems[0].contains("gc.log.lock"), "{problems:?}");
        assert!(git.join("gc.log.lock").exists(), "reporting removes nothing");

        let f = inspect(&git, &|_| false, AGE);
        let done = fix(&git, &f, 0);
        assert!(!git.join("gc.log.lock").exists() && !git.join("gc.pid").exists(), "{done:?}");
        let cfg = Command::new("git").arg("--git-dir").arg(&git).args(["config", "gc.autoDetach"]).output().unwrap();
        assert_eq!(String::from_utf8_lossy(&cfg.stdout).trim(), "false");
        assert!(done.iter().any(|s| s == "ran git gc"), "{done:?}");
        assert!(git.join("objects/pack").read_dir().unwrap().count() > 0, "packed");
        let _ = std::fs::remove_dir_all(&d);
    }

    #[test]
    fn a_live_gc_is_left_alone() {
        let d = repo("live");
        let git = d.join(".git");
        plant(&git);
        let f = inspect(&git, &|_| true, AGE);
        assert!(f.live && !f.wants_attention());
        assert!(fix(&git, &f, 0).is_empty());
        assert!(git.join("gc.log.lock").exists() && git.join("gc.pid").exists());
        let _ = std::fs::remove_dir_all(&d);
    }

    #[test]
    fn a_commit_in_flight_holds_the_pack_back_but_not_the_unlocking() {
        let d = repo("busy");
        let git = d.join(".git");
        plant(&git);
        std::fs::write(git.join("index.lock"), "").unwrap();
        let f = inspect(&git, &|_| false, AGE);
        let done = fix(&git, &f, 0);
        assert!(!git.join("gc.log.lock").exists());
        assert!(done.iter().any(|s| s.starts_with("not packing")), "{done:?}");
        assert_eq!(git.join("objects/pack").read_dir().unwrap().count(), 0);
        let _ = std::fs::remove_dir_all(&d);
    }

    #[test]
    fn a_clean_repository_is_not_mentioned() {
        let d = repo("clean");
        let f = inspect(&d.join(".git"), &alive, AGE);
        assert!(!f.wants_attention(), "{f:?}");
        let _ = std::fs::remove_dir_all(&d);
    }
}
