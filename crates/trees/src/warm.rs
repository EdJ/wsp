//! A new tree arrives warm: its parent's `node_modules` and cargo `target/`,
//! cloned copy-on-write (wsp-355).
//!
//! `git worktree add` brings only what is tracked, so every fresh tree used to
//! start with a cold `npm install` and a cold cargo build. Each dependency
//! directory beside a lockfile whose bytes match the parent's is cloned with
//! **one `clonefile(2)` call on the directory** — 0.68s for 13,113 entries and
//! 219M on compounding-048, against 26.4s for `cp -c -R`, which clones per file.
//! Blocks are shared until written, so a leg that adds a dependency changes
//! only its own copy. Never a symlink: Node and Vite resolve real paths, and a
//! link would let one leg's install change its siblings'.
//!
//! Anything short of a match copies nothing and says so in one line; the agent
//! installs or builds as it did before.
//!
//! **The clone is cheap because the source is kept small** (wsp-393). A clone
//! costs by entry, and the trunk's `target/debug/deps` held 157,745 leaked
//! `*.rcgu.o` codegen objects among 4,000 real files: 70s to clone, 0.57s with
//! them gone. They are rustc's intermediates, unused once linked, so those older
//! than [`STALE`] are removed from the parent's `target/` before it is cloned
//! ([`prune_objects`]; the first pass is long, so it stops at [`PRUNE_FOR`] and
//! the next tree carries on).
//!
//! **And the wait is bounded.** The clone is made under a hidden name outside
//! the tree and renamed in only when it finishes inside [`LIMIT`]; past that the
//! tree starts cold, the line says so, and the half-made copy is removed once
//! this process has gone.

use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

/// The longest a tree waits on one clone: half a cold build of wsp (about 170s),
/// for past that a cold start is the cheaper one (an 88s clone is waited out, a
/// 223s one is not).
const LIMIT: Duration = Duration::from_secs(90);

/// A leaked codegen object this old is not a build's in flight.
const STALE: Duration = Duration::from_secs(6 * 60 * 60);

/// How long one tree spends pruning before it clones what is left.
const PRUNE_FOR: Duration = Duration::from_secs(20);

/// The dependency directories worth cloning, each with the lockfile that must
/// match beside it.
const KINDS: &[(&str, &str)] = &[("package-lock.json", "node_modules"), ("Cargo.lock", "target")];

/// How deep under the tree's root a lockfile is looked for.
const DEPTH: usize = 6;

/// Directories never walked into: dependency and build output, and other trees.
const SKIP: &[&str] = &[".git", ".worktrees", "node_modules", "target", "dist", "build", ".next", ".venv"];

/// Clone `parent`'s dependency directories into the fresh tree at `child`, and
/// say what happened, one line each. Nothing at all when the parent has no
/// lockfile with its directory beside it — the first tree has nothing to give,
/// and the line about installing there is the order's, not this one's.
pub fn clone_from(parent: &Path, child: &Path) -> Vec<String> {
    let mut found = Vec::new();
    lockfiles(parent, Path::new(""), 0, &mut found);
    found.sort();
    found.into_iter().map(|(rel, dir)| one(parent, child, &rel, dir)).collect()
}

fn lockfiles(root: &Path, rel: &Path, depth: usize, out: &mut Vec<(PathBuf, &'static str)>) {
    let here = root.join(rel);
    for (lock, dir) in KINDS {
        if here.join(lock).is_file() && here.join(dir).is_dir() {
            out.push((rel.join(dir), lock));
        }
    }
    if depth >= DEPTH {
        return;
    }
    let Ok(entries) = std::fs::read_dir(&here) else { return };
    for e in entries.flatten() {
        let name = e.file_name();
        let name = name.to_string_lossy();
        if SKIP.contains(&name.as_ref()) || name.starts_with('.') {
            continue;
        }
        // Not through a symlink: a linked directory is somebody else's tree.
        if e.file_type().is_ok_and(|t| t.is_dir()) {
            lockfiles(root, &rel.join(&*name), depth + 1, out);
        }
    }
}

/// `rel` is the dependency directory, relative to both trees; `lock` the file
/// beside it that must match.
fn one(parent: &Path, child: &Path, rel: &Path, lock: &str) -> String {
    let shown = rel.display();
    let lock_rel = rel.parent().unwrap_or(Path::new("")).join(lock);
    let theirs = std::fs::read(parent.join(&lock_rel));
    let ours = std::fs::read(child.join(&lock_rel));
    match (theirs, ours) {
        (Ok(a), Ok(b)) if a == b => {}
        (_, Err(_)) => return format!("{shown} not cloned: this tree has no {} — install as usual", lock_rel.display()),
        _ => return format!("{shown} not cloned: {} differs from the parent's — install as usual", lock_rel.display()),
    }
    let to = child.join(rel);
    if to.exists() {
        return format!("{shown} not cloned: already present");
    }
    let from = parent.join(rel);
    let pruned = if lock == "Cargo.lock" { prune_objects(&from, STALE, PRUNE_FOR) } else { 0 };
    let note = if pruned > 0 { format!(" (after removing {pruned} stale codegen objects from it)") } else { String::new() };
    let started = Instant::now();
    // Beside the tree, not in it: `git status` never sees the half-made copy.
    let staging = child.parent().unwrap_or(child);
    let hidden = format!("{}-{}", child.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default(), rel.to_string_lossy().replace('/', "_"));
    match bounded(&from, &to, &staging.join(format!(".{hidden}.cloning-{}", std::process::id())), LIMIT, clone_dir) {
        Ok(()) => format!("{shown} cloned from the parent tree in {:.2}s{note}", started.elapsed().as_secs_f64()),
        Err(e) => format!("{shown} not cloned ({e}) — install as usual"),
    }
}

/// Remove the `*.rcgu.o` files older than `older` from every `<target>/<profile>/deps`,
/// stopping after `spend`, and say how many went. Rustc writes these per codegen
/// unit and removes them after linking; a build that is killed leaves them, and
/// they are most of the entries in a trunk `target/` that has seen many.
fn prune_objects(target: &Path, older: Duration, spend: Duration) -> usize {
    let started = Instant::now();
    let now = std::time::SystemTime::now();
    let mut gone = 0;
    let Ok(profiles) = std::fs::read_dir(target) else { return 0 };
    for profile in profiles.flatten() {
        let Ok(files) = std::fs::read_dir(profile.path().join("deps")) else { continue };
        for f in files.flatten() {
            if started.elapsed() > spend {
                return gone;
            }
            if !f.file_name().to_string_lossy().ends_with(".rcgu.o") {
                continue;
            }
            let aged = f.metadata().ok().and_then(|m| m.modified().ok()).and_then(|t| now.duration_since(t).ok()).is_some_and(|age| age > older);
            if aged && std::fs::remove_file(f.path()).is_ok() {
                gone += 1;
            }
        }
    }
    gone
}

/// Run `clone` from `from` to the hidden name `tmp`, and move it into place at
/// `to` only if it finishes inside `limit`. Past the limit the copy is left to the
/// thread still making it and a shell removes it once this process is gone.
fn bounded<F>(from: &Path, to: &Path, tmp: &Path, limit: Duration, clone: F) -> Result<(), String>
where
    F: FnOnce(&Path, &Path) -> Result<(), String> + Send + 'static,
{
    let (src, dst, tmp) = (from.to_path_buf(), tmp.to_path_buf(), tmp.to_path_buf());
    let (tx, rx) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        let _ = tx.send(clone(&src, &dst));
    });
    match rx.recv_timeout(limit) {
        Ok(Ok(())) => std::fs::rename(&tmp, to).map_err(|e| {
            let _ = std::fs::remove_dir_all(&tmp);
            e.to_string()
        }),
        Ok(Err(e)) => Err(e),
        Err(_) => {
            remove_when_gone(&tmp);
            Err(format!("still cloning after {}s, so the tree starts without it", limit.as_secs()))
        }
    }
}

/// Remove `path` after this process has exited, which is when the clone still
/// running in it has stopped. Best effort: a leftover is only disk.
fn remove_when_gone(path: &Path) {
    let script = format!("while kill -0 {} 2>/dev/null; do sleep 1; done; rm -rf \"$1\"", std::process::id());
    let _ = std::process::Command::new("sh")
        .args(["-c", &script, "sh"])
        .arg(path)
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn();
}

#[cfg(target_os = "macos")]
fn clone_dir(from: &Path, to: &Path) -> Result<(), String> {
    use std::ffi::CString;
    use std::os::raw::{c_char, c_int, c_uint};
    use std::os::unix::ffi::OsStrExt;
    extern "C" {
        fn clonefile(src: *const c_char, dst: *const c_char, flags: c_uint) -> c_int;
    }
    /// Copy symlinks as links rather than following them (`CLONE_NOFOLLOW`).
    const CLONE_NOFOLLOW: c_uint = 0x0001;
    let src = CString::new(from.as_os_str().as_bytes()).map_err(|e| e.to_string())?;
    let dst = CString::new(to.as_os_str().as_bytes()).map_err(|e| e.to_string())?;
    if let Some(up) = to.parent() {
        std::fs::create_dir_all(up).map_err(|e| e.to_string())?;
    }
    // SAFETY: both pointers are NUL-terminated strings that outlive the call.
    let rc = unsafe { clonefile(src.as_ptr(), dst.as_ptr(), CLONE_NOFOLLOW) };
    if rc == 0 { Ok(()) } else { Err(std::io::Error::last_os_error().to_string()) }
}

#[cfg(not(target_os = "macos"))]
fn clone_dir(_from: &Path, _to: &Path) -> Result<(), String> {
    Err("clonefile is macOS only".to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn scratch(name: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("wsp-warm-{}-{name}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        d
    }

    fn write(p: &Path, s: &str) {
        std::fs::create_dir_all(p.parent().unwrap()).unwrap();
        std::fs::write(p, s).unwrap();
    }

    #[test]
    #[cfg(target_os = "macos")]
    fn a_matching_lockfile_brings_its_dependencies_and_build_output_along() {
        let d = scratch("match");
        let (parent, child) = (d.join("p"), d.join("c"));
        write(&parent.join("web/package-lock.json"), "{1}");
        write(&parent.join("web/node_modules/vite/index.js"), "vite");
        write(&parent.join("Cargo.lock"), "lock");
        write(&parent.join("target/debug/x"), "bin");
        write(&child.join("web/package-lock.json"), "{1}");
        write(&child.join("Cargo.lock"), "lock");
        let lines = clone_from(&parent, &child);
        assert_eq!(lines.len(), 2, "{lines:?}");
        assert!(lines.iter().all(|l| l.contains("cloned from the parent")), "{lines:?}");
        assert_eq!(std::fs::read_to_string(child.join("web/node_modules/vite/index.js")).unwrap(), "vite");
        assert!(!child.join("web/node_modules").symlink_metadata().unwrap().file_type().is_symlink());
        // A write in the clone leaves the parent's copy alone.
        std::fs::write(child.join("target/debug/x"), "changed").unwrap();
        assert_eq!(std::fs::read_to_string(parent.join("target/debug/x")).unwrap(), "bin");
        let _ = std::fs::remove_dir_all(&d);
    }

    #[test]
    fn a_clone_that_runs_past_the_limit_is_given_up_and_leaves_nothing_in_place() {
        let d = scratch("slow");
        let to = d.join("c/target");
        std::fs::create_dir_all(d.join("c")).unwrap();
        let slow = |_: &Path, t: &Path| {
            std::thread::sleep(Duration::from_millis(400));
            std::fs::create_dir_all(t).map_err(|e| e.to_string())
        };
        let err = bounded(&d.join("p"), &to, &d.join(".t.cloning"), Duration::from_millis(20), slow).unwrap_err();
        assert!(err.contains("still cloning after"), "{err}");
        assert!(!to.exists(), "a late clone is never moved into place");
        let _ = std::fs::remove_dir_all(&d);
    }

    #[test]
    fn a_clone_that_finishes_in_time_is_moved_into_place() {
        let d = scratch("quick");
        let to = d.join("c/target");
        std::fs::create_dir_all(d.join("c")).unwrap();
        let quick = |_: &Path, t: &Path| std::fs::create_dir_all(t).map_err(|e| e.to_string());
        bounded(&d.join("p"), &to, &d.join(".t.cloning"), Duration::from_secs(5), quick).unwrap();
        assert!(to.is_dir());
        assert_eq!(std::fs::read_dir(&d).unwrap().filter(|e| e.as_ref().unwrap().file_name().to_string_lossy().contains("cloning")).count(), 0, "no hidden name left");
        let _ = std::fs::remove_dir_all(&d);
    }

    #[test]
    fn stale_codegen_objects_go_before_the_clone_and_nothing_else_does() {
        let d = scratch("prune");
        let deps = d.join("target/debug/deps");
        write(&deps.join("old.rcgu.o"), "o");
        write(&deps.join("new.rcgu.o"), "o");
        write(&deps.join("old.rlib"), "r");
        let aged = std::time::SystemTime::now() - Duration::from_secs(24 * 60 * 60);
        for n in ["old.rcgu.o", "old.rlib"] {
            std::fs::File::options().write(true).open(deps.join(n)).unwrap().set_modified(aged).unwrap();
        }
        assert_eq!(prune_objects(&d.join("target"), STALE, Duration::from_secs(5)), 1);
        assert!(!deps.join("old.rcgu.o").exists(), "a day-old leaked object goes");
        assert!(deps.join("new.rcgu.o").exists(), "a build in flight keeps its own");
        assert!(deps.join("old.rlib").exists(), "only codegen objects are pruned");
        assert_eq!(prune_objects(&d.join("target"), STALE, Duration::ZERO), 0, "out of time, it stops");
        let _ = std::fs::remove_dir_all(&d);
    }

    #[test]
    fn the_wait_for_a_clone_is_less_than_the_cold_build_it_saves() {
        // A cold build of wsp is about 170s; waiting longer than that for the
        // clone is worse than starting without it.
        assert!(LIMIT < Duration::from_secs(170) && LIMIT >= Duration::from_secs(60));
    }

    #[test]
    fn a_clone_is_staged_outside_the_tree_it_is_for() {
        let d = scratch("staged");
        let (parent, child) = (d.join("p"), d.join("trees/c"));
        write(&parent.join("Cargo.lock"), "lock");
        write(&parent.join("target/debug/x"), "bin");
        write(&child.join("Cargo.lock"), "lock");
        let lines = clone_from(&parent, &child);
        assert_eq!(lines.len(), 1, "{lines:?}");
        if cfg!(target_os = "macos") {
            assert!(lines[0].contains("cloned from the parent"), "{lines:?}");
            assert!(child.join("target/debug/x").exists());
        }
        let hidden: Vec<_> = std::fs::read_dir(&child).unwrap().flatten().filter(|e| e.file_name().to_string_lossy().contains("cloning")).collect();
        assert!(hidden.is_empty(), "nothing hidden in the tree");
        let _ = std::fs::remove_dir_all(&d);
    }

    #[test]
    fn a_tree_with_no_lockfile_beside_a_parents_target_is_a_note_and_nothing_else() {
        let d = scratch("nolock");
        let (parent, child) = (d.join("p"), d.join("c"));
        write(&parent.join("spikes/canvas/Cargo.lock"), "lock");
        write(&parent.join("spikes/canvas/target/debug/x"), "bin");
        write(&child.join("spikes/canvas/src/lib.rs"), "");
        let lines = clone_from(&parent, &child);
        assert_eq!(lines, vec!["spikes/canvas/target not cloned: this tree has no spikes/canvas/Cargo.lock — install as usual"]);
        assert!(!child.join("spikes/canvas/target").exists());
        let _ = std::fs::remove_dir_all(&d);
    }

    #[test]
    fn a_lockfile_that_differs_copies_nothing_and_says_so() {
        let d = scratch("differs");
        let (parent, child) = (d.join("p"), d.join("c"));
        write(&parent.join("package-lock.json"), "{1}");
        write(&parent.join("node_modules/a"), "a");
        write(&child.join("package-lock.json"), "{2}");
        let lines = clone_from(&parent, &child);
        assert_eq!(lines, vec!["node_modules not cloned: package-lock.json differs from the parent's — install as usual"]);
        assert!(!child.join("node_modules").exists());
        let _ = std::fs::remove_dir_all(&d);
    }

    #[test]
    fn a_parent_with_nothing_installed_says_nothing() {
        let d = scratch("empty");
        let (parent, child) = (d.join("p"), d.join("c"));
        write(&parent.join("package-lock.json"), "{1}");
        write(&child.join("package-lock.json"), "{1}");
        assert!(clone_from(&parent, &child).is_empty());
        let _ = std::fs::remove_dir_all(&d);
    }
}
