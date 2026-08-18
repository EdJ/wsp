//! What every build on this machine shares: a few warm build trees, and the
//! cores.
//!
//! Isolation between trees was bought deliberately — `27f1a18` gave every
//! checkout a build tree of its own so that a green build means *your* HEAD is
//! green, and [`crate::cmd_verify`] is the long argument for it. What was never
//! decided is what those trees share, and the answer was nothing at all:
//! measured 2026-08-17 with five agents and two governors on one laptop, load
//! averages 468, twenty-one `rustc`, and Ed sitting at it. Every new task got a
//! new tree, every new tree was cold, and every `cargo` read the core count and
//! took `-j8` as though it owned the box.
//!
//! # The cold build is the cost, and warmth is the fix
//!
//! Measured 2026-08-18, this crate, one tree, `cargo test --no-run`:
//!
//! - empty target directory: **23s**
//! - the same tree reset to HEAD, somebody else's patch applied, built again:
//!   **3s**, and 3s again for a patch touching a different file
//!
//! Nearly eight to one, and it is not this crate being unusually small — the
//! `fork` project gives every task a worktree off a 266-crate repository, where
//! the ratio is worse. So what is shared here is the **build tree itself**: a
//! few per repository, at fixed paths under the state directory, each handed to
//! one build at a time.
//!
//! Exclusive is what makes it safe. The disaster in [`crate::cmd_verify`]'s
//! header — `Fresh wsp v0.1.0` for source that was never compiled — came from
//! *two* trees pointed at one target directory, where cargo's fingerprints
//! record absolute paths and answer for the wrong tree. One tree with one
//! target directory, reset to your HEAD and carrying your patch before it
//! builds, is the arrangement every developer's own checkout already is: cargo
//! judges freshness on the files it is about to compile, and they are yours.
//!
//! Nobody waits for one. If every tree is busy the build falls back to the
//! private cold one it would have had anyway — warmth when there is some, and
//! today's behaviour when there is not, because a verify an agent has to queue
//! for is a verify it stops running.
//!
//! # Why not a compiler cache, which was the obvious answer
//!
//! `sccache` was the first candidate on `robustness-070`, and it was measured
//! before being believed. It cannot do this. Two worktrees of the same commit,
//! same compiler, same flags, built one after the other into one `SCCACHE_DIR`:
//! **0 hits, 5 misses**. Give the *second* tree the *first* tree's target
//! directory path and the same five compilations hit 5 out of 5.
//!
//! What its key turns on is therefore the target directory, which is precisely
//! what two isolated builds cannot share: cargo puts `-L dependency=<target>/…`
//! and `OUT_DIR=<target>/…` on the rustc command line and they are in the hash.
//! `SCCACHE_BASEDIR` does not rescue it — also measured, also 0 hits. On a full
//! miss the wrapper costs about 8% (90–95s against 82–86s, four builds at
//! once), so a cache that cannot be hit is worse than no cache, and there is
//! none here.
//!
//! # The budget, which is the other half
//!
//! Warm trees bound how much work there is; they do not bound how much of the
//! machine one build helps itself to. So every build registers while it runs
//! and takes `cores / builds` jobs — six agents produce eight compile jobs
//! between them rather than forty-eight. Ed's standing cap of four working
//! agents exists because of that arithmetic, and it caps the wrong thing: four
//! agents *thinking* cost nothing, and four agents *building* is what
//! saturates.
//!
//! It is deliberately not a machine-wide lock. One verify at a time is simpler
//! and makes an agent wait ten minutes for cores the holder is not using while
//! its tests run.
//!
//! The share is fixed when the build starts: a build that starts alone keeps
//! the machine even when four more arrive behind it, because recomputing
//! mid-build means stopping cargo. The overshoot is bounded and decays — seven
//! staggered starts on eight cores ask for 8, 4, 2, 2, 1, 1, 1, nineteen
//! against the fifty-six that were measured — and the build that piles on is
//! always the small one, which is the right way round.
//!
//! # What it does not touch, and that is a judgement
//!
//! `cargo test` runs 615 tests with a thread per core, and that is load too: on
//! four concurrent verifies the load peak arrived *after* the last `rustc`
//! exited. It is left alone because test parallelism is not a resource knob
//! here — it changes what the suite exercises. `robustness-072` measured a test
//! that fails 48 times in 50 run alone and passes on a loaded machine, because
//! the contention was handing a child process the 20ms it needed. Rescheduling
//! the tests quietly would move that ground while other tasks stand on it.

use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::process::Command;

/// How many warm trees a repository gets.
///
/// Three rather than one, so the usual two or three verifies in flight are all
/// warm; and rather than one per agent, because each is a whole target
/// directory — 295M for this crate, several gigabytes for a large one — and the
/// leak this replaces was thirty trees and 9.6G. Beyond three, a build takes
/// the private cold tree rather than waiting.
pub(crate) const WARM_TREES: usize = 3;

/// A warm build tree, held for the length of one build.
///
/// The lock is the whole point: two builds in one tree at once would be the
/// shared-checkout failure this arrangement exists to prevent, arriving from
/// the other side.
pub(crate) struct Warm {
    /// The directory holding `tree` and `target`.
    pub dir: PathBuf,
    /// Which one it is, for saying so out loud.
    pub slot: usize,
    lock: PathBuf,
}

impl Drop for Warm {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.lock);
    }
}

impl Warm {
    /// The worktree that gets reset to the caller's HEAD and patched.
    pub(crate) fn tree(&self) -> PathBuf {
        self.dir.join("tree")
    }

    /// Its `CARGO_TARGET_DIR`, which is the part worth keeping.
    pub(crate) fn target(&self) -> PathBuf {
        self.dir.join("target")
    }
}

/// Take a warm tree for this repository, or `None` when they are all busy.
///
/// Keyed on the repository rather than on the agent or the task, which is the
/// whole change: an agent is new every spawn and a task is new every day, so
/// either key guarantees a cold build. The repository is the thing that stays.
pub(crate) fn warm(state: &Path, repo: &Path, slots: usize) -> Option<Warm> {
    let name = repo.file_name().and_then(|s| s.to_str()).unwrap_or("repo");
    claim(&state.join("warm"), &crate::util::slugify(name), slots, alive)
}

/// Every warm tree for this repository that nobody is building in.
///
/// For `wsp verify --rm`, which is how a tree that has gone wrong gets thrown
/// away. Held ones are left where they are: a build standing in a tree is the
/// one thing that must not have it removed underneath it, and the next `--rm`
/// will find it free.
pub(crate) fn warm_each(state: &Path, repo: &Path, slots: usize) -> Vec<Warm> {
    let name = repo.file_name().and_then(|s| s.to_str()).unwrap_or("repo");
    let root = state.join("warm");
    let repo = crate::util::slugify(name);
    (0..slots).filter_map(|n| claim_at(&root, &repo, n, alive)).collect()
}

/// One named warm tree, if nobody is building in it.
///
/// For `wsp verify --rm`, which knows exactly which tree it wants: the one this
/// agent's last build went to, read back off the pointer that build left. A
/// held tree comes back `None` and stays where it is.
pub(crate) fn warm_named(dir: &Path) -> Option<Warm> {
    let root = dir.parent()?;
    let name = dir.file_name()?.to_str()?;
    let (repo, slot) = name.rsplit_once('-')?;
    claim_at(root, repo, slot.parse().ok()?, alive)
}

/// The claim itself, with liveness passed in so that it can be tested.
fn claim(
    root: &Path,
    repo: &str,
    slots: usize,
    alive: impl Fn(&[u32]) -> HashSet<u32>,
) -> Option<Warm> {
    (0..slots).find_map(|slot| claim_at(root, repo, slot, &alive))
}

/// One named tree, taken or not.
fn claim_at(
    root: &Path,
    repo: &str,
    slot: usize,
    alive: impl Fn(&[u32]) -> HashSet<u32>,
) -> Option<Warm> {
    let _ = std::fs::create_dir_all(root);
    let lock = root.join(format!("{repo}-{slot}.lock"));
    // Two passes at most: take it, or find out whose it is and whether that
    // process is still there. A lock left by a build that was killed would
    // otherwise take a tree out of circulation for good, and the trees are the
    // thing being rationed.
    for attempt in 0..2 {
        match std::fs::OpenOptions::new().write(true).create_new(true).open(&lock) {
            Ok(mut f) => {
                use std::io::Write;
                let _ = writeln!(f, "{}", std::process::id());
                // The lock is made, the directory is not: whoever builds
                // here creates it, and `--rm` wants to know the difference
                // between a tree it removed and a slot that was never used.
                return Some(Warm { dir: root.join(format!("{repo}-{slot}")), slot, lock });
            }
            Err(_) if attempt == 0 => {
                let held = std::fs::read_to_string(&lock).ok().and_then(|s| s.trim().parse().ok());
                match held {
                    Some(pid) if alive(&[pid]).contains(&pid) => return None,
                    // Nobody is behind it: drop it and go round once. If
                    // another build wins that race, the second `create_new`
                    // fails and this tree is simply somebody else's.
                    _ => {
                        let _ = std::fs::remove_file(&lock);
                    }
                }
            }
            Err(_) => return None,
        }
    }
    None
}

/// A registration that lasts as long as the build does, and the share of the
/// machine it was given.
///
/// Held by the caller for the length of the build and dropped after it, which
/// is the whole lifetime: a registration that outlived its build would make
/// every later build smaller for nothing.
pub(crate) struct Share {
    /// Cargo jobs this build may run at once.
    pub jobs: usize,
    /// What the machine has.
    pub cores: usize,
    /// Builds registered when this one started, including this one.
    pub live: usize,
    /// This build's registration, removed on drop.
    slot: Option<PathBuf>,
}

impl Drop for Share {
    fn drop(&mut self) {
        if let Some(p) = &self.slot {
            let _ = std::fs::remove_file(p);
        }
    }
}

/// Register this build and work out what it may take.
///
/// The registration is a file named for the pid, which is what lets a build
/// killed with `-9` be cleaned up by the next one: nothing is left to run a
/// release step, so liveness has to be readable from outside.
///
/// Under the state directory rather than some machine-wide path, which is the
/// bargain every other machine-local fact wsp keeps makes. It costs a `wsp
/// sandbox` its own budget, and that is the right way round: a sandbox exists
/// so that nothing it does reaches what the real agents read.
pub(crate) fn take(state: &Path) -> Share {
    let cores = std::thread::available_parallelism().map(|n| n.get()).unwrap_or(1);
    let dir = state.join("building");
    let _ = std::fs::create_dir_all(&dir);
    let slot = dir.join(std::process::id().to_string());
    // The file's name is what matters; what is in it is for whoever is standing
    // in `~/.local/state/wsp/building` wondering what these are.
    let ok = std::fs::write(&slot, format!("started {}\n", crate::util::now_iso())).is_ok();
    // Counted after registering, so this build is in the count. Two builds
    // starting at the same instant both see two and both take half, which is
    // the answer that does not overshoot; counting first would have them both
    // see one and both take everything.
    let live = live_slots(&dir, alive).max(1);
    let asked = wanted(std::env::var("CARGO_BUILD_JOBS").ok().as_deref());
    Share { jobs: asked.unwrap_or_else(|| share_of(cores, live)), cores, live, slot: ok.then_some(slot) }
}

impl Share {
    /// Put the share on a cargo command.
    ///
    /// `CARGO_BUILD_JOBS` rather than `--jobs`, because the argv is the thing
    /// this command prints and an agent reading `cargo test --quiet -j2` in the
    /// output would reasonably try to reproduce it by typing it.
    pub(crate) fn apply(&self, cmd: &mut Command) {
        cmd.env("CARGO_BUILD_JOBS", self.jobs.to_string());
    }

    /// What the machine gave this build, when that is worth a line.
    ///
    /// It is printed on every verify, so it earns its width: a build running at
    /// a quarter of the cores is the answer to "why did this take four minutes
    /// today and one minute yesterday", which is what the swing has been
    /// costing. A machine to itself says nothing at all.
    pub(crate) fn note(&self) -> Option<String> {
        (self.live > 1)
            .then(|| format!("{} of {} jobs · {} builds here", self.jobs, self.cores, self.live))
    }
}

/// One job, and no registration.
///
/// For tests that reach a `cargo` call they never intend to let run: the
/// registration is the only part with a side effect, and a test that took a
/// real one would be counted by every build running beside it.
#[cfg(test)]
pub(crate) fn unregistered() -> Share {
    Share { jobs: 1, cores: 1, live: 1, slot: None }
}

/// A job count the caller asked for out loud, if it asked.
///
/// The budget is a default and not a cap: an agent that exported
/// `CARGO_BUILD_JOBS` before running this has said something deliberate, and
/// overriding it would leave no way to measure the budget, or to hand one build
/// the machine on purpose. What it stops is the *unknowing* saturation, which
/// is every build that says nothing.
fn wanted(env: Option<&str>) -> Option<usize> {
    env?.trim().parse::<usize>().ok().filter(|n| *n > 0)
}

/// What one build may take when `live` of them are running.
///
/// Integer division and a floor of one: five builds on eight cores get one job
/// each and leave three cores for the tests running beside them, which is
/// closer to right than rounding up and asking for ten.
fn share_of(cores: usize, live: usize) -> usize {
    (cores / live.max(1)).max(1)
}

/// How many builds are registered and still running.
///
/// Dead registrations are removed rather than merely skipped, because the pids
/// that leave them come from builds that were killed, and a machine that has
/// been used for a week would otherwise divide its cores by its history.
///
/// `alive` is passed in for the one reason that matters here: the arithmetic is
/// worth a test and spawning processes to make one is not.
fn live_slots(dir: &Path, alive: impl Fn(&[u32]) -> HashSet<u32>) -> usize {
    let Ok(entries) = std::fs::read_dir(dir) else { return 0 };
    let mut found: Vec<(u32, PathBuf)> = Vec::new();
    for e in entries.flatten() {
        let path = e.path();
        let Some(pid) = path.file_name().and_then(|s| s.to_str()).and_then(|s| s.parse().ok())
        else {
            continue;
        };
        // Older than a day is dead whatever the pid says. Pids are reused, and
        // a leaked file whose number came round again would otherwise count a
        // long-lived daemon as a build for as long as that daemon ran.
        let stale = e
            .metadata()
            .ok()
            .and_then(|m| m.modified().ok())
            .and_then(|t| t.elapsed().ok())
            .is_some_and(|age| age.as_secs() > 24 * 60 * 60);
        if stale {
            let _ = std::fs::remove_file(&path);
            continue;
        }
        found.push((pid, path));
    }
    let pids: Vec<u32> = found.iter().map(|(p, _)| *p).collect();
    let live = alive(&pids);
    let mut n = 0;
    for (pid, path) in found {
        if live.contains(&pid) {
            n += 1;
        } else {
            let _ = std::fs::remove_file(&path);
        }
    }
    n
}

/// Which of these pids still exist.
///
/// One `ps` for the whole list rather than one signal per pid: the list is
/// short, but it is read at the start of every build and a process per entry is
/// a cost that grows with the thing being counted.
///
/// A `ps` that did not run is not evidence that the processes are gone, so the
/// failure keeps everything. A build counted that has already finished costs
/// the next one some jobs; a tree taken from a build still standing in it costs
/// correctness.
fn alive(pids: &[u32]) -> HashSet<u32> {
    if pids.is_empty() {
        return HashSet::new();
    }
    let csv = pids.iter().map(|p| p.to_string()).collect::<Vec<_>>().join(",");
    let out = Command::new("ps").args(["-o", "pid=", "-p", &csv]).output();
    let Ok(out) = out else { return pids.iter().copied().collect() };
    out.stdout
        .split(|b| *b == b'\n')
        .filter_map(|l| String::from_utf8_lossy(l).trim().parse().ok())
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Everything is alive, which is the ordinary case and the one a test of
    /// the claim itself wants out of the way.
    fn all(pids: &[u32]) -> HashSet<u32> {
        pids.iter().copied().collect()
    }

    #[test]
    fn one_build_alone_takes_the_whole_machine() {
        assert_eq!(share_of(8, 1), 8);
    }

    #[test]
    fn builds_divide_the_cores_between_them() {
        assert_eq!(share_of(8, 2), 4);
        assert_eq!(share_of(8, 4), 2);
    }

    #[test]
    fn a_crowded_machine_still_gives_every_build_a_job() {
        assert_eq!(share_of(8, 20), 1);
        assert_eq!(share_of(1, 3), 1);
    }

    #[test]
    fn a_build_that_says_what_it_wants_is_obeyed() {
        assert_eq!(wanted(Some("3")), Some(3));
        assert_eq!(wanted(Some(" 12 ")), Some(12));
    }

    #[test]
    fn a_job_count_that_is_not_one_falls_back_to_the_budget() {
        assert_eq!(wanted(None), None);
        assert_eq!(wanted(Some("")), None);
        assert_eq!(wanted(Some("0")), None, "nought jobs is not a build");
        assert_eq!(wanted(Some("many")), None);
    }

    #[test]
    fn a_registration_whose_process_has_gone_is_removed_rather_than_counted() {
        let iso = crate::util::isolated("sharing-dead");
        let dir = iso.path("building");
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("101"), "").unwrap();
        std::fs::write(dir.join("102"), "").unwrap();
        let live = live_slots(&dir, |_| HashSet::from([101]));
        assert_eq!(live, 1, "only the live pid counts");
        assert!(!dir.join("102").exists(), "the dead one is cleaned up, not left to be recounted");
        assert!(dir.join("101").exists());
    }

    #[test]
    fn a_file_that_is_not_a_pid_is_not_a_build() {
        let iso = crate::util::isolated("sharing-junk");
        let dir = iso.path("building");
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("notes.txt"), "").unwrap();
        assert_eq!(live_slots(&dir, |_| HashSet::new()), 0);
        assert!(dir.join("notes.txt").exists(), "somebody else's file is left where it was");
    }

    #[test]
    fn taking_a_share_registers_this_build_and_releases_it_on_drop() {
        let iso = crate::util::isolated("sharing-slot");
        let state = iso.state();
        let slot = state.join("building").join(std::process::id().to_string());
        // No job count in the environment, or this measures whoever ran the
        // suite rather than the budget. `isolated` holds the environment lock,
        // so no other test can be looking while it is unset.
        let asked = std::env::var_os("CARGO_BUILD_JOBS");
        std::env::remove_var("CARGO_BUILD_JOBS");
        {
            let share = take(&state);
            assert!(slot.exists(), "a running build is visible to the next one");
            assert!(share.live >= 1 && share.jobs >= 1);
            assert!(share.jobs <= share.cores);
        }
        if let Some(j) = asked {
            std::env::set_var("CARGO_BUILD_JOBS", j);
        }
        assert!(!slot.exists(), "and stops being visible when it finishes");
    }

    #[test]
    fn a_machine_to_itself_has_nothing_to_report() {
        let share = Share { jobs: 8, cores: 8, live: 1, slot: None };
        assert_eq!(share.note(), None);
    }

    #[test]
    fn a_shared_machine_says_what_it_is_sharing_with() {
        let share = Share { jobs: 2, cores: 8, live: 4, slot: None };
        let note = share.note().unwrap();
        assert!(note.contains("2 of 8"), "{note}");
        assert!(note.contains("4 builds here"), "{note}");
    }

    #[test]
    fn two_builds_in_one_repository_get_a_warm_tree_each() {
        let iso = crate::util::isolated("warm-two");
        let root = iso.path("warm");
        let first = claim(&root, "wsp", 3, all).unwrap();
        let second = claim(&root, "wsp", 3, all).unwrap();
        assert_ne!(first.dir, second.dir, "one tree cannot hold two builds");
        assert!(first.target().starts_with(&first.dir));
    }

    #[test]
    fn a_build_beyond_the_last_warm_tree_is_told_no_rather_than_made_to_wait() {
        let iso = crate::util::isolated("warm-full");
        let root = iso.path("warm");
        let held: Vec<Warm> = (0..2).filter_map(|_| claim(&root, "wsp", 2, all)).collect();
        assert_eq!(held.len(), 2);
        assert!(
            claim(&root, "wsp", 2, all).is_none(),
            "the third build takes the private cold tree it would have had anyway"
        );
    }

    #[test]
    fn a_tree_is_free_again_when_the_build_holding_it_finishes() {
        let iso = crate::util::isolated("warm-release");
        let root = iso.path("warm");
        let dir = claim(&root, "wsp", 1, all).unwrap().dir.clone();
        let again = claim(&root, "wsp", 1, all).expect("released on drop");
        assert_eq!(again.dir, dir, "and it is the same tree, which is the point of it");
    }

    #[test]
    fn a_tree_locked_by_a_build_that_died_is_taken_back() {
        let iso = crate::util::isolated("warm-dead");
        let root = iso.path("warm");
        std::fs::create_dir_all(&root).unwrap();
        std::fs::write(root.join("wsp-0.lock"), "424242\n").unwrap();
        let got = claim(&root, "wsp", 1, |_| HashSet::new()).expect("a dead lock is not a holder");
        assert!(got.dir.ends_with("wsp-0"));
    }

    #[test]
    fn a_tree_locked_by_a_build_that_is_still_running_is_left_alone() {
        let iso = crate::util::isolated("warm-held");
        let root = iso.path("warm");
        std::fs::create_dir_all(&root).unwrap();
        std::fs::write(root.join("wsp-0.lock"), "424242\n").unwrap();
        assert!(
            claim(&root, "wsp", 1, |_| HashSet::from([424242])).is_none(),
            "somebody else is building in it"
        );
    }

    #[test]
    fn the_tree_a_build_went_to_can_be_named_and_taken_back() {
        let iso = crate::util::isolated("warm-named");
        let root = iso.path("warm");
        let dir = {
            let held = claim(&root, "wsp", 3, all).unwrap();
            held.dir.clone()
        };
        let again = warm_named(&dir).expect("the pointer names a free tree");
        assert_eq!(again.dir, dir);
        assert!(warm_named(&dir).is_none(), "and it is held while somebody holds it");
    }

    #[test]
    fn a_pointer_at_something_that_is_not_a_warm_tree_names_nothing() {
        let iso = crate::util::isolated("warm-nonsense");
        assert!(warm_named(&iso.path("warm/wsp-nine")).is_none(), "no slot number in it");
        assert!(warm_named(&iso.path("warm")).is_none());
    }

    #[test]
    fn two_repositories_do_not_take_each_others_trees() {
        let iso = crate::util::isolated("warm-repos");
        let root = iso.path("warm");
        let mine = claim(&root, "wsp", 1, all).unwrap();
        let theirs = claim(&root, "herdr", 1, all).unwrap();
        assert_ne!(mine.dir, theirs.dir, "a tree warm for one crate is cold for another");
    }
}
