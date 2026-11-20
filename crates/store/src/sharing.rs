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
//! # The compiler cache, which sits beside the warm trees rather than replacing them
//!
//! `sccache` was the first candidate on `robustness-070` and it measured badly:
//! two worktrees of the same commit built into one `SCCACHE_DIR` gave **0 hits,
//! 5 misses**, and the same five compilations hit 5 out of 5 once the second
//! tree was given the first tree's target directory path. Its key turned on the
//! tree, because cargo puts `-L dependency=<target>/…` and `OUT_DIR=<target>/…`
//! on the rustc command line. `SCCACHE_BASEDIR` did not rescue it.
//!
//! That is no longer true of a build here, because `.cargo/config.toml` routes
//! every rustc through `.cargo/rustc-wrap` (compound-965, wsp-343), which
//! removes the tree from the key: a proc-macro is linked with `-Wl,-S` and a
//! fixed install name, a registry crate's `OUT_DIR` is copied to a directory
//! named by its contents, a registry proc-macro is served by `rustc-pm`'s own
//! content-addressed cache, and `rustc-remap` adds `--remap-path-prefix` after
//! sccache has keyed the compile. A fresh tree's first build therefore hits the
//! cache for registry dependencies. Why each step exists, and what it measured,
//! is in compound's `docs/build-cache.md`.
//!
//! The cache holds *registry* crates, which is where a cold build's time goes;
//! the workspace's own crate is compiled in every tree, since cargo builds it
//! incrementally and sccache refuses that. So the warm trees above still matter
//! for the part a compiler cache cannot serve, and they are what keeps cargo's
//! fingerprints right for a patched tree.
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

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};
use wsp_base::calls::Command;

/// Which of these pids are running: the host's answer (`wire::host::Host::alive`),
/// passed in because this layer sits below the host and asks it nothing itself.
/// A host that cannot tell keeps every pid, so a failed read reaps no claim.
pub type Alive<'a> = &'a dyn Fn(&[u32]) -> BTreeSet<u32>;

/// One `wsp verify` run under `WSP_JOBS=manager` (wsp-054): the host its jobs
/// run on, and the row, attempt and mark every one of its job keys carries
/// (`verify:<what>:<n>@<mark>`). It is what a warm lock and a build's
/// registration name in place of a pid, so whether its holder is still there
/// is read from that host's job table and never asked with `kill -0`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Run {
    pub host: String,
    pub row: String,
    pub attempt: u32,
    pub mark: String,
}

impl Run {
    /// The line it is written into a lock as. Tabs between the parts, which
    /// no host id, row or mark contains.
    pub fn line(&self) -> String {
        format!("job\t{}\t{}\t{}\t{}", self.host, self.row, self.attempt, self.mark)
    }

    /// A lock's or a registration's text, read back: the first `job` line.
    pub fn parse(text: &str) -> Option<Run> {
        let line = text.lines().find(|l| l.starts_with("job\t"))?;
        let mut parts = line.split('\t').skip(1);
        let (host, row, attempt, mark) = (parts.next()?, parts.next()?, parts.next()?.parse().ok()?, parts.next()?);
        Some(Run { host: host.into(), row: row.into(), attempt, mark: mark.into() })
    }

    /// Whether a job key is one of this run's jobs.
    pub fn owns(&self, row: &str, attempt: u32, job: &str) -> bool {
        row == self.row && attempt == self.attempt && job.rsplit_once('@').is_some_and(|(_, m)| m == self.mark)
    }
}

/// One row of a host's job table, as much of it as liveness reads.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Seen {
    pub row: String,
    pub attempt: u32,
    pub job: String,
    /// When it started, in seconds since the epoch.
    pub started: u64,
    pub running: bool,
}

/// A host's job table, asked by host id: `None` when it could not be read,
/// and then every holder on that host is kept, as a failed pid read keeps
/// every pid.
pub type Table<'a> = &'a dyn Fn(&str) -> Option<Vec<Seen>>;

/// How long a run's claim is held with no job of its own running: the setup
/// between taking a warm tree and its first `job.run` (the worktree, the reset,
/// the patch), which runs no job for the table to show. A run touches its lock
/// before each job ([`Warm::touch`]), and a job of the run that started since
/// that touch and has ended frees it at once; this bound is only for a run
/// killed before it ran anything.
pub const SETUP_GRACE: std::time::Duration = std::time::Duration::from_secs(10 * 60);

/// Older than this, a holder of the *other* choice's kind is dead whatever it
/// says: a pid lock read under `manager`, or a run's lock read under `inproc`,
/// which neither can ask about. The same day [`live_slots`] already allows.
const STALE: std::time::Duration = std::time::Duration::from_secs(24 * 60 * 60);

/// How a lock's holder is asked whether it is still there.
#[derive(Clone, Copy)]
pub enum Live<'a> {
    /// `WSP_JOBS=inproc`: the lock holds this process's pid, asked of the host.
    Pids(Alive<'a>),
    /// `WSP_JOBS=manager`: the lock holds `me`, and a holder is read from its
    /// host's job table.
    Jobs { me: &'a Run, table: Table<'a> },
}

impl Live<'_> {
    /// What this process writes into a lock it takes.
    fn mine(&self) -> String {
        match self {
            Live::Pids(_) => std::process::id().to_string(),
            Live::Jobs { me, .. } => me.line(),
        }
    }

    /// Whether the holder `text` names, its file last touched at `touched`, is
    /// still there.
    fn holds(&self, text: &str, touched: Option<std::time::SystemTime>) -> bool {
        let age = touched.and_then(|t| t.elapsed().ok());
        let pid: Option<u32> = text.lines().next().and_then(|l| l.trim().parse().ok());
        match (self, pid, Run::parse(text)) {
            (Live::Pids(alive), Some(pid), _) => alive(&[pid]).contains(&pid),
            (Live::Jobs { table, .. }, None, Some(run)) => run_holds(&run, touched, table),
            // The other choice's holder, or nothing readable: kept until a day
            // old, because nothing here can ask it.
            _ => age.is_some_and(|a| a < STALE) && (pid.is_some() || Run::parse(text).is_some()),
        }
    }

    /// Where this choice keeps its warm trees: one pool per host under
    /// `manager` (6.2, "M3 under T2"), the machine's one under `inproc`.
    fn warm_root(&self, state: &Path) -> PathBuf {
        match self {
            Live::Pids(_) => state.join("warm"),
            Live::Jobs { me, .. } => state.join("warm").join(crate::util::slugify(&me.host)),
        }
    }
}

/// Whether `run` still holds what it locked, by its host's job table: one of
/// its jobs is running; or none has started since the lock was last touched
/// and that touch is within [`SETUP_GRACE`]. A job that started since the
/// touch and has ended, with none running, is a run that finished or was
/// killed, and its lock is free.
fn run_holds(run: &Run, touched: Option<std::time::SystemTime>, table: Table) -> bool {
    let Some(rows) = table(&run.host) else { return true };
    let mine: Vec<&Seen> = rows.iter().filter(|s| run.owns(&s.row, s.attempt, &s.job)).collect();
    if mine.iter().any(|s| s.running) {
        return true;
    }
    let Some(touched) = touched else { return false };
    let since = touched.duration_since(std::time::UNIX_EPOCH).map_or(0, |d| d.as_secs());
    if mine.iter().any(|s| s.started >= since) {
        return false;
    }
    touched.elapsed().is_ok_and(|a| a < SETUP_GRACE)
}

/// When a file was last written, if it can be read.
fn touched(path: &Path) -> Option<std::time::SystemTime> {
    std::fs::metadata(path).ok()?.modified().ok()
}

/// How many warm trees a repository gets.
///
/// Three rather than one, so the usual two or three verifies in flight are all
/// warm; and rather than one per agent, because each is a whole target
/// directory — 295M for this crate, several gigabytes for a large one — and the
/// leak this replaces was thirty trees and 9.6G. Beyond three, a build takes
/// the private cold tree rather than waiting.
pub const WARM_TREES: usize = 3;

/// A warm build tree, held for the length of one build.
///
/// The lock is the whole point: two builds in one tree at once would be the
/// shared-checkout failure this arrangement exists to prevent, arriving from
/// the other side.
pub struct Warm {
    /// The directory holding `tree` and `target`.
    pub dir: PathBuf,
    /// Which one it is, for saying so out loud.
    pub slot: usize,
    lock: PathBuf,
    /// What was written into the lock, to know it is still this one's.
    holder: String,
}

impl Drop for Warm {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.lock);
    }
}

impl Warm {
    /// The worktree that gets reset to the caller's HEAD and patched.
    pub fn tree(&self) -> PathBuf {
        self.dir.join("tree")
    }

    /// Its `CARGO_TARGET_DIR`, which is the part worth keeping.
    pub fn target(&self) -> PathBuf {
        self.dir.join("target")
    }

    /// Say the tree is still held, just before a job runs in it: the lock's
    /// time is what [`run_holds`] reads, so a job that starts after it and
    /// ends frees the tree, and the setup before the first one is covered.
    /// `false` when the lock is no longer this one's — another build took the
    /// tree while nothing of this run was running in it — and then the caller
    /// must not run there.
    pub fn touch(&self) -> bool {
        if std::fs::read_to_string(&self.lock).map(|t| t.trim() == self.holder).unwrap_or(false) {
            let f = std::fs::OpenOptions::new().append(true).open(&self.lock);
            return f.and_then(|f| f.set_modified(std::time::SystemTime::now())).is_ok();
        }
        false
    }
}

/// Take a warm tree for this repository, or `None` when they are all busy.
///
/// Keyed on the repository rather than on the agent or the task, which is the
/// whole change: an agent is new every spawn and a task is new every day, so
/// either key guarantees a cold build. The repository is the thing that stays.
pub fn warm(state: &Path, repo: &Path, slots: usize, live: Live) -> Option<Warm> {
    let name = repo.file_name().and_then(|s| s.to_str()).unwrap_or("repo");
    claim(&live.warm_root(state), &crate::util::slugify(name), slots, live)
}

/// Every warm tree for this repository that nobody is building in.
///
/// For `wsp verify --rm`, which is how a tree that has gone wrong gets thrown
/// away. Held ones are left where they are: a build standing in a tree is the
/// one thing that must not have it removed underneath it, and the next `--rm`
/// will find it free.
pub fn warm_each(state: &Path, repo: &Path, slots: usize, live: Live) -> Vec<Warm> {
    let name = repo.file_name().and_then(|s| s.to_str()).unwrap_or("repo");
    let root = live.warm_root(state);
    let repo = crate::util::slugify(name);
    (0..slots).filter_map(|n| claim_at(&root, &repo, n, live)).collect()
}

/// One named warm tree, if nobody is building in it.
///
/// For `wsp verify --rm`, which knows exactly which tree it wants: the one this
/// agent's last build went to, read back off the pointer that build left. A
/// held tree comes back `None` and stays where it is.
pub fn warm_named(dir: &Path, live: Live) -> Option<Warm> {
    let root = dir.parent()?;
    let name = dir.file_name()?.to_str()?;
    let (repo, slot) = name.rsplit_once('-')?;
    claim_at(root, repo, slot.parse().ok()?, live)
}

/// The claim itself, with liveness passed in so that it can be tested.
fn claim(root: &Path, repo: &str, slots: usize, live: Live) -> Option<Warm> {
    (0..slots).find_map(|slot| claim_at(root, repo, slot, live))
}

/// One named tree, taken or not.
fn claim_at(root: &Path, repo: &str, slot: usize, live: Live) -> Option<Warm> {
    let _ = std::fs::create_dir_all(root);
    let lock = root.join(format!("{repo}-{slot}.lock"));
    // Two passes at most: take it, or find out whose it is and whether that
    // process is still there. A lock left by a build that was killed would
    // otherwise take a tree out of circulation for good, and the trees are the
    // thing being rationed. Under `manager` the lock names the verify run and
    // "still there" is its host's job table ([`run_holds`]).
    let holder = live.mine();
    for attempt in 0..2 {
        match std::fs::OpenOptions::new().write(true).create_new(true).open(&lock) {
            Ok(mut f) => {
                use std::io::Write;
                let _ = writeln!(f, "{holder}");
                // The lock is made, the directory is not: whoever builds
                // here creates it, and `--rm` wants to know the difference
                // between a tree it removed and a slot that was never used.
                return Some(Warm { dir: root.join(format!("{repo}-{slot}")), slot, lock, holder });
            }
            Err(_) if attempt == 0 => {
                let text = std::fs::read_to_string(&lock).unwrap_or_default();
                if live.holds(&text, touched(&lock)) {
                    return None;
                }
                // Nobody is behind it: drop it and go round once. If
                // another build wins that race, the second `create_new`
                // fails and this tree is simply somebody else's.
                let _ = std::fs::remove_file(&lock);
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
pub struct Share {
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
pub fn take(state: &Path, live: Live) -> Share {
    let cores = std::thread::available_parallelism().map(|n| n.get()).unwrap_or(1);
    let dir = state.join("building");
    let _ = std::fs::create_dir_all(&dir);
    // Under `inproc` the file's name is what matters, a pid; what is in it is
    // for whoever is standing in `~/.local/state/wsp/building` wondering what
    // these are. Under `manager` it is named for the run and says which run it
    // is, which its host's job table answers for.
    let (slot, body) = match live {
        Live::Pids(_) => (dir.join(std::process::id().to_string()), String::new()),
        Live::Jobs { me, .. } => (dir.join(format!("run-{}", crate::util::slugify(&me.mark))), format!("{}\n", me.line())),
    };
    let ok = std::fs::write(&slot, format!("started {}\n{body}", crate::util::now_iso())).is_ok();
    // Counted after registering, so this build is in the count. Two builds
    // starting at the same instant both see two and both take half, which is
    // the answer that does not overshoot; counting first would have them both
    // see one and both take everything.
    let live = live_slots(&dir, live).max(1);
    let asked = wanted(std::env::var("CARGO_BUILD_JOBS").ok().as_deref());
    Share { jobs: asked.unwrap_or_else(|| share_of(cores, live)), cores, live, slot: ok.then_some(slot) }
}

impl Share {
    /// Put the share on a cargo command.
    ///
    /// `CARGO_BUILD_JOBS` rather than `--jobs`, because the argv is the thing
    /// this command prints and an agent reading `cargo test --quiet -j2` in the
    /// output would reasonably try to reproduce it by typing it.
    pub fn apply(&self, cmd: &mut Command) {
        let (k, v) = self.env();
        cmd.env(k, v);
    }

    /// The same, as the `K=V` a job's env prefix carries under
    /// `WSP_JOBS=manager`.
    pub fn env(&self) -> (&'static str, String) {
        ("CARGO_BUILD_JOBS", self.jobs.to_string())
    }

    /// What the machine gave this build, when that is worth a line.
    ///
    /// It is printed on every verify, so it earns its width: a build running at
    /// a quarter of the cores is the answer to "why did this take four minutes
    /// today and one minute yesterday", which is what the swing has been
    /// costing. A machine to itself says nothing at all.
    pub fn note(&self) -> Option<String> {
        (self.live > 1)
            .then(|| format!("{} of {} jobs · {} builds here", self.jobs, self.cores, self.live))
    }
}

/// One job, and no registration.
///
/// For tests that reach a `cargo` call they never intend to let run: the
/// registration is the only part with a side effect, and a test that took a
/// real one would be counted by every build running beside it.
#[cfg(any(test, feature = "test-support"))]
pub fn unregistered() -> Share {
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
/// `live` is passed in for the one reason that matters here: the arithmetic is
/// worth a test and spawning processes to make one is not.
fn live_slots(dir: &Path, live: Live) -> usize {
    let Ok(entries) = std::fs::read_dir(dir) else { return 0 };
    let mut pids: Vec<(u32, PathBuf)> = Vec::new();
    let mut runs: Vec<PathBuf> = Vec::new();
    for e in entries.flatten() {
        let path = e.path();
        let Some(name) = path.file_name().and_then(|s| s.to_str()) else { continue };
        let pid: Option<u32> = name.parse().ok();
        if pid.is_none() && !name.starts_with("run-") {
            continue;
        }
        // Older than a day is dead whatever the pid says. Pids are reused, and
        // a leaked file whose number came round again would otherwise count a
        // long-lived daemon as a build for as long as that daemon ran.
        let stale = e
            .metadata()
            .ok()
            .and_then(|m| m.modified().ok())
            .and_then(|t| t.elapsed().ok())
            .is_some_and(|age| age > STALE);
        if stale {
            let _ = std::fs::remove_file(&path);
            continue;
        }
        match pid {
            Some(pid) => pids.push((pid, path)),
            None => runs.push(path),
        }
    }
    let mut n = 0;
    // The pids in one question, as before: the host is asked once a call.
    let running = match live {
        Live::Pids(alive) => Some(alive(&pids.iter().map(|(p, _)| *p).collect::<Vec<_>>())),
        Live::Jobs { .. } => None,
    };
    for (pid, path) in pids {
        let held = match &running {
            Some(set) => set.contains(&pid),
            None => live.holds(&pid.to_string(), touched(&path)),
        };
        if held {
            n += 1;
        } else {
            let _ = std::fs::remove_file(&path);
        }
    }
    for path in runs {
        let text = std::fs::read_to_string(&path).unwrap_or_default();
        if live.holds(&text, touched(&path)) {
            n += 1;
        } else {
            let _ = std::fs::remove_file(&path);
        }
    }
    n
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Everything is alive, which is the ordinary case and the one a test of
    /// the claim itself wants out of the way.
    fn all(pids: &[u32]) -> BTreeSet<u32> {
        pids.iter().copied().collect()
    }

    fn none(_: &[u32]) -> BTreeSet<u32> {
        BTreeSet::new()
    }

    const ALL: Live<'static> = Live::Pids(&all);

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
        let live = live_slots(&dir, Live::Pids(&|_: &[u32]| BTreeSet::from([101])));
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
        assert_eq!(live_slots(&dir, Live::Pids(&none)), 0);
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
            let share = take(&state, ALL);
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
        let first = claim(&root, "wsp", 3, ALL).unwrap();
        let second = claim(&root, "wsp", 3, ALL).unwrap();
        assert_ne!(first.dir, second.dir, "one tree cannot hold two builds");
        assert!(first.target().starts_with(&first.dir));
    }

    #[test]
    fn a_build_beyond_the_last_warm_tree_is_told_no_rather_than_made_to_wait() {
        let iso = crate::util::isolated("warm-full");
        let root = iso.path("warm");
        let held: Vec<Warm> = (0..2).filter_map(|_| claim(&root, "wsp", 2, ALL)).collect();
        assert_eq!(held.len(), 2);
        assert!(
            claim(&root, "wsp", 2, ALL).is_none(),
            "the third build takes the private cold tree it would have had anyway"
        );
    }

    #[test]
    fn a_tree_is_free_again_when_the_build_holding_it_finishes() {
        let iso = crate::util::isolated("warm-release");
        let root = iso.path("warm");
        let dir = claim(&root, "wsp", 1, ALL).unwrap().dir.clone();
        let again = claim(&root, "wsp", 1, ALL).expect("released on drop");
        assert_eq!(again.dir, dir, "and it is the same tree, which is the point of it");
    }

    #[test]
    fn a_tree_locked_by_a_build_that_died_is_taken_back() {
        let iso = crate::util::isolated("warm-dead");
        let root = iso.path("warm");
        std::fs::create_dir_all(&root).unwrap();
        std::fs::write(root.join("wsp-0.lock"), "424242\n").unwrap();
        let got = claim(&root, "wsp", 1, Live::Pids(&none)).expect("a dead lock is not a holder");
        assert!(got.dir.ends_with("wsp-0"));
    }

    #[test]
    fn a_tree_locked_by_a_build_that_is_still_running_is_left_alone() {
        let iso = crate::util::isolated("warm-held");
        let root = iso.path("warm");
        std::fs::create_dir_all(&root).unwrap();
        std::fs::write(root.join("wsp-0.lock"), "424242\n").unwrap();
        assert!(
            claim(&root, "wsp", 1, Live::Pids(&|_: &[u32]| BTreeSet::from([424242]))).is_none(),
            "somebody else is building in it"
        );
    }

    #[test]
    fn the_tree_a_build_went_to_can_be_named_and_taken_back() {
        let iso = crate::util::isolated("warm-named");
        let root = iso.path("warm");
        let dir = {
            let held = claim(&root, "wsp", 3, ALL).unwrap();
            held.dir.clone()
        };
        let again = warm_named(&dir, ALL).expect("the pointer names a free tree");
        assert_eq!(again.dir, dir);
        assert!(warm_named(&dir, ALL).is_none(), "and it is held while somebody holds it");
    }

    #[test]
    fn a_pointer_at_something_that_is_not_a_warm_tree_names_nothing() {
        let iso = crate::util::isolated("warm-nonsense");
        assert!(warm_named(&iso.path("warm/wsp-nine"), ALL).is_none(), "no slot number in it");
        assert!(warm_named(&iso.path("warm"), ALL).is_none());
    }

    #[test]
    fn two_repositories_do_not_take_each_others_trees() {
        let iso = crate::util::isolated("warm-repos");
        let root = iso.path("warm");
        let mine = claim(&root, "wsp", 1, ALL).unwrap();
        let theirs = claim(&root, "backend", 1, ALL).unwrap();
        assert_ne!(mine.dir, theirs.dir, "a tree warm for one crate is cold for another");
    }

    // ---- under `WSP_JOBS=manager` (wsp-054): a holder is a run, read from
    // its host's job table, never a pid.

    fn run(mark: &str) -> Run {
        Run { host: "h".into(), row: "wsp-054".into(), attempt: 0, mark: mark.into() }
    }

    fn seen(job: &str, started: u64, running: bool) -> Seen {
        Seen { row: "wsp-054".into(), attempt: 0, job: job.into(), started, running }
    }

    fn now() -> u64 {
        std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_secs()
    }

    fn backdate(path: &Path, by: std::time::Duration) {
        let f = std::fs::OpenOptions::new().append(true).open(path).unwrap();
        f.set_modified(std::time::SystemTime::now() - by).unwrap();
    }

    #[test]
    fn a_run_reads_back_from_its_line_and_owns_only_its_own_jobs() {
        let r = run("17-4");
        assert_eq!(Run::parse(&format!("started x\n{}\n", r.line())), Some(r.clone()));
        assert!(r.owns("wsp-054", 0, "verify:cargo-test:0@17-4"));
        assert!(!r.owns("wsp-054", 0, "verify:cargo-test:0@17-5"), "another run's");
        assert!(!r.owns("wsp-054", 1, "verify:cargo-test:0@17-4"), "another attempt's");
        assert_eq!(Run::parse("424242\n"), None);
    }

    #[test]
    fn a_tree_whose_runs_build_is_running_is_held_and_free_once_it_ended() {
        let iso = crate::util::isolated("warm-jobs");
        let root = iso.path("warm");
        let (a, b) = (run("a"), run("b"));
        let rows: std::cell::RefCell<Vec<Seen>> = Default::default();
        let table = |_: &str| Some(rows.borrow().clone());
        let held = claim(&root, "wsp", 1, Live::Jobs { me: &a, table: &table }).unwrap();
        assert!(std::fs::read_to_string(root.join("wsp-0.lock")).unwrap().contains("\ta"), "the lock names the run");
        assert!(claim(&root, "wsp", 1, Live::Jobs { me: &b, table: &table }).is_none(), "held through setup");

        *rows.borrow_mut() = vec![seen("verify:cargo-test:0@a", now(), true)];
        assert!(claim(&root, "wsp", 1, Live::Jobs { me: &b, table: &table }).is_none(), "held while it builds");

        // Killed: the job ended and nothing removed the lock.
        *rows.borrow_mut() = vec![seen("verify:cargo-test:0@a", now(), false)];
        std::mem::forget(held);
        let got = claim(&root, "wsp", 1, Live::Jobs { me: &b, table: &table }).expect("free by the table");
        assert!(got.touch(), "the new run holds it");
    }

    #[test]
    fn a_run_killed_before_its_first_job_holds_its_tree_only_through_the_setup_grace() {
        let iso = crate::util::isolated("warm-setup");
        let root = iso.path("warm");
        let (a, b) = (run("a"), run("b"));
        let empty = |_: &str| Some(Vec::new());
        std::mem::forget(claim(&root, "wsp", 1, Live::Jobs { me: &a, table: &empty }).unwrap());
        assert!(claim(&root, "wsp", 1, Live::Jobs { me: &b, table: &empty }).is_none());
        backdate(&root.join("wsp-0.lock"), SETUP_GRACE + std::time::Duration::from_secs(5));
        assert!(claim(&root, "wsp", 1, Live::Jobs { me: &b, table: &empty }).is_some(), "past the grace with no job");
    }

    #[test]
    fn a_table_that_cannot_be_read_keeps_every_holder() {
        let iso = crate::util::isolated("warm-silent");
        let root = iso.path("warm");
        let (a, b) = (run("a"), run("b"));
        let silent = |_: &str| None;
        std::mem::forget(claim(&root, "wsp", 1, Live::Jobs { me: &a, table: &silent }).unwrap());
        backdate(&root.join("wsp-0.lock"), SETUP_GRACE * 2);
        assert!(claim(&root, "wsp", 1, Live::Jobs { me: &b, table: &silent }).is_none(), "silence is not a dead run");
    }

    #[test]
    fn a_lock_of_the_other_choice_is_kept_until_a_day_old_and_never_asked() {
        let iso = crate::util::isolated("warm-other");
        let root = iso.path("warm");
        let b = run("b");
        let asked = std::cell::Cell::new(0);
        let table = |_: &str| {
            asked.set(asked.get() + 1);
            Some(Vec::new())
        };
        std::fs::create_dir_all(&root).unwrap();
        std::fs::write(root.join("wsp-0.lock"), "424242\n").unwrap();
        assert!(claim(&root, "wsp", 1, Live::Jobs { me: &b, table: &table }).is_none(), "a pid lock is not probed");
        backdate(&root.join("wsp-0.lock"), STALE + std::time::Duration::from_secs(5));
        assert!(claim(&root, "wsp", 1, Live::Jobs { me: &b, table: &table }).is_some());
        assert_eq!(asked.get(), 0, "no table was asked about a pid");

        std::fs::write(root.join("wsp-1.lock"), format!("{}\n", run("a").line())).unwrap();
        assert!(claim_at(&root, "wsp", 1, Live::Pids(&none)).is_none(), "a run's lock is kept under inproc");
    }

    #[test]
    fn under_manager_a_build_registration_counts_while_its_run_lives() {
        let iso = crate::util::isolated("sharing-jobs");
        let dir = iso.path("building");
        std::fs::create_dir_all(&dir).unwrap();
        let (a, b) = (run("a"), run("b"));
        std::fs::write(dir.join("run-a"), format!("started x\n{}\n", a.line())).unwrap();
        std::fs::write(dir.join("run-b"), format!("started x\n{}\n", b.line())).unwrap();
        let rows = vec![seen("verify:cargo-test:0@a", now(), true), seen("verify:cargo-test:0@b", now() + 1, false)];
        let table = |_: &str| Some(rows.clone());
        assert_eq!(live_slots(&dir, Live::Jobs { me: &run("c"), table: &table }), 1);
        assert!(dir.join("run-a").exists());
        assert!(!dir.join("run-b").exists(), "the ended run's registration is removed");
    }
}
