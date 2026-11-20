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
//! this process has gone. Under `WSP_JOBS=manager` that removal is a detached
//! job on the manager of the host that holds the copy ([`removal_job`]), not a
//! shell of wsp's own.

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
/// thread still making it, and is removed once this process is gone
/// ([`remove_when_gone`]); under `manager`, by this process too if the clone
/// ends while it lives.
fn bounded<F>(from: &Path, to: &Path, tmp: &Path, limit: Duration, clone: F) -> Result<(), String>
where
    F: FnOnce(&Path, &Path) -> Result<(), String> + Send + 'static,
{
    let (src, dst, tmp) = (from.to_path_buf(), tmp.to_path_buf(), tmp.to_path_buf());
    let (tx, rx) = std::sync::mpsc::channel();
    // What a clone that ends after the limit leaves behind, once it is given
    // up: taken under this lock, so a clone ends either before the give-up
    // (and is moved in) or after it (and is tidied), never between.
    let late: std::sync::Arc<std::sync::Mutex<Option<Tidy>>> = std::sync::Arc::default();
    let given_up = late.clone();
    std::thread::spawn(move || {
        let done = clone(&src, &dst);
        let mut late = given_up.lock().unwrap_or_else(|e| e.into_inner());
        match late.take() {
            Some(tidy) => tidy.now(&dst),
            None => {
                let _ = tx.send(done);
            }
        }
    });
    let finish = |done: Result<(), String>| match done {
        Ok(()) => std::fs::rename(&tmp, to).map_err(|e| {
            let _ = std::fs::remove_dir_all(&tmp);
            e.to_string()
        }),
        Err(e) => Err(e),
    };
    match rx.recv_timeout(limit) {
        Ok(done) => finish(done),
        Err(_) => {
            let mut late = late.lock().unwrap_or_else(|e| e.into_inner());
            if let Ok(done) = rx.try_recv() {
                return finish(done);
            }
            *late = Some(remove_when_gone(&tmp));
            Err(format!("still cloning after {}s, so the tree starts without it", limit.as_secs()))
        }
    }
}

/// How a given-up clone is removed, and what is left for its thread to do if
/// it ends while this process still lives.
enum Tidy {
    /// Nothing removes it once the process is gone: the removal job could not
    /// be placed or sent, and a leftover is only disk. The thread does nothing.
    Left,
    /// A detached job removes it once the process is gone ([`removal_job`]).
    /// A clone that ends first is removed here and the job let go: `hold` is
    /// what the job waits on, and closing it ends the wait.
    Job { fifo: PathBuf, hold: std::fs::File },
}

impl Tidy {
    /// The clone at `dst` has ended after it was given up.
    fn now(self, dst: &Path) {
        if let Tidy::Job { fifo, hold } = self {
            let _ = std::fs::remove_dir_all(dst);
            let _ = std::fs::remove_file(&fifo);
            drop(hold);
        }
    }
}

/// The longest warm's removal job waits for this process to go. It holds one
/// of its host's build slots while it waits, so it is bounded: a process that
/// outlives it removes the copy itself when the clone ends ([`Tidy::now`]).
const REMOVAL_WAIT: Duration = Duration::from_secs(15 * 60);

/// How long [`removal_job`] waits to hear its job start before it lets the
/// caller go on.
const REMOVAL_HEARD: Duration = Duration::from_secs(10);

/// Remove `path` after this process has exited, which is when the clone still
/// running in it has stopped: a detached job on the host the jobs go to (the
/// manager's, or `wsp-host`'s runner in this process under `WSP_JOBS=inproc`
/// and where there is no manager), placed where the clone is. Where that job
/// cannot be placed or sent nothing removes it. Best effort: a leftover is
/// only disk.
fn remove_when_gone(path: &Path) -> Tidy {
    let state = std::env::var_os("WSP_STATE").map(PathBuf::from).unwrap_or_else(crate::store::Store::default_state);
    if let Ok(host) = crate::throttle::jobs_host(&state) {
        if let Ok((fifo, hold)) = removal_job(host, path) {
            return Tidy::Job { fifo, hold };
        }
    }
    Tidy::Left
}

/// The removal of the given-up clone at `path` as a **detached job** on
/// `host`'s manager, which outlives this process: placed in the clone's
/// staging directory relative to the host's tree root, it waits on a FIFO
/// beside the clone that this process holds open (the returned file) and
/// removes the clone and the FIFO once it reads the end of it, which comes
/// when every holder has closed it: when this process has gone, or when
/// [`Tidy::now`] lets it go. No pid is probed by wsp or by the job.
///
/// Answers the FIFO and its hold once the job is heard to start, or once
/// [`REMOVAL_HEARD`] passes with no answer (a job slow to start is still the
/// removal, and dropping the hold would let it remove a clone still in
/// flight); an error where it could not be placed, was refused or ended.
fn removal_job(host: &'static dyn crate::host::Host, path: &Path) -> Result<(PathBuf, std::fs::File), String> {
    let (Some(parent), Some(name)) = (path.parent(), path.file_name().map(|n| n.to_string_lossy().into_owned())) else {
        return Err(format!("{} has no directory to stand in", path.display()));
    };
    let at = crate::job_place::dir_on(&crate::util::hostname(), parent, "the warm clone's staging directory")?;
    let held = format!("{name}.held");
    let fifo = parent.join(&held);
    let _ = std::fs::remove_file(&fifo);
    make_fifo(&fifo)?;
    // Read and write, so the open does not wait for the job's reader; closed
    // on exec, so nothing this process starts holds it past its end.
    let hold = std::fs::File::options().read(true).write(true).open(&fifo).map_err(|e| {
        let _ = std::fs::remove_file(&fifo);
        format!("{}: {e}", fifo.display())
    })?;
    let run = removal_run(at, &held, &name);
    let (tx, rx) = std::sync::mpsc::channel::<Result<(), String>>();
    std::thread::spawn(move || {
        let heard = tx.clone();
        let ended = host.run_job(&run, &mut |_| {
            let _ = heard.send(Ok(()));
        });
        let _ = tx.send(Err(match ended {
            Ok(end) => format!("the removal job ended before it held (exit {:?})", end.exit),
            Err(e) => format!("the removal job was not run: {e}"),
        }));
    });
    match rx.recv_timeout(REMOVAL_HEARD) {
        Ok(Ok(())) | Err(_) => Ok((fifo, hold)),
        Ok(Err(why)) => {
            drop(hold);
            let _ = std::fs::remove_file(&fifo);
            Err(why)
        }
    }
}

/// The `job.run` of [`removal_job`]: detached, so it outlives the connection
/// (and the process) that sent it; at the staging directory `at`; waiting on
/// the FIFO `held` and then removing it and the clone `name`, both named
/// relative to `at`, within [`REMOVAL_WAIT`].
fn removal_run(at: crate::host::HostDir, held: &str, name: &str) -> crate::host::JobRun {
    let nanos = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0, |d| d.as_nanos());
    crate::host::JobRun {
        key: crate::host::job_key("warm", 0, &format!("remove:{name}@{nanos}")),
        class: crate::host::JobClass::Build,
        argv: ["sh", "-c", "echo held; cat \"$1\" > /dev/null; rm -rf -- \"$2\" \"$1\"", "sh", held, name].map(String::from).to_vec(),
        at: crate::host::JobAt::Dir(at),
        timeout_secs: REMOVAL_WAIT.as_secs(),
        hung_after_secs: None,
        detach: true,
    }
}

/// Make a FIFO at `path`, readable and writable by its owner.
fn make_fifo(path: &Path) -> Result<(), String> {
    use std::os::raw::{c_char, c_int};
    use std::os::unix::ffi::OsStrExt;
    #[cfg(target_os = "macos")]
    type Mode = u16;
    #[cfg(not(target_os = "macos"))]
    type Mode = u32;
    extern "C" {
        fn mkfifo(path: *const c_char, mode: Mode) -> c_int;
    }
    let c = std::ffi::CString::new(path.as_os_str().as_bytes()).map_err(|e| e.to_string())?;
    // SAFETY: a NUL-terminated string that outlives the call.
    let rc = unsafe { mkfifo(c.as_ptr(), 0o600) };
    if rc == 0 { Ok(()) } else { Err(format!("{}: {}", path.display(), std::io::Error::last_os_error())) }
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

    /// Under `manager` a given-up clone's removal is a detached job at the
    /// staging directory on the host that holds it: it outlives the
    /// connection that sent it, waits while its caller lives, and removes the
    /// clone once the caller has gone (its hold closed). A clone that ends
    /// while its caller lives is removed by the caller and the job let go.
    #[test]
    fn a_given_up_clone_is_removed_by_a_detached_job_once_its_caller_has_gone() {
        use crate::host::{Host, JobAt, JobClass, JobState};
        let d = scratch("job").canonicalize().unwrap();
        crate::job_place::set_tree_root(Some(d.clone()));
        let host: &'static crate::host::InProcess = Box::leak(Box::new(crate::host::InProcess::new().with_jobs(&d.join("jobs"), &d)));
        let until = |what: &str, ok: &dyn Fn() -> bool| {
            let end = Instant::now() + Duration::from_secs(15);
            while !ok() {
                assert!(Instant::now() < end, "{what}");
                std::thread::sleep(Duration::from_millis(50));
            }
        };
        let tmp = d.join("stage/.c-target.cloning-1");
        write(&tmp.join("debug/x"), "half");

        let at = crate::job_place::dir_on(&crate::util::hostname(), tmp.parent().unwrap(), "x").unwrap();
        let run = removal_run(at.clone(), ".c-target.cloning-1.held", ".c-target.cloning-1");
        assert!(run.detach, "outlives the connection that sent it");
        assert_eq!((run.at, run.class, at.path.as_str(), at.host.0.clone()), (JobAt::Dir(at.clone()), JobClass::Build, "stage", crate::util::hostname()));
        assert!(!run.argv.join(" ").contains(&*d.to_string_lossy()), "no path on wsp's machine: {:?}", run.argv);

        let (fifo, hold) = removal_job(host, &tmp).unwrap();
        let rows = host.jobs().unwrap();
        assert_eq!(rows.len(), 1, "{rows:?}");
        assert_eq!((rows[0].key.row.0.as_str(), rows[0].state), ("warm", JobState::Running));
        std::thread::sleep(Duration::from_millis(800));
        assert!(tmp.exists() && fifo.exists(), "nothing is removed while its caller lives");
        drop(hold);
        until("the job removed the clone once its caller had gone", &|| !tmp.exists() && !fifo.exists());
        until("the job ended", &|| host.jobs().unwrap().iter().all(|r| r.state == JobState::Ended));

        let tmp = d.join("stage/.c-node_modules.cloning-1");
        write(&tmp.join("a"), "half");
        let (fifo, hold) = removal_job(host, &tmp).unwrap();
        Tidy::Job { fifo: fifo.clone(), hold }.now(&tmp);
        assert!(!tmp.exists() && !fifo.exists(), "ended while its caller lives: removed here");
        until("the job was let go", &|| host.jobs().unwrap().iter().all(|r| r.state == JobState::Ended));
        assert!(removal_job(host, Path::new("/x")).is_err(), "a clone outside the tree root is no job's");
        crate::job_place::set_tree_root(None);
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
