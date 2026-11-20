//! `wsp verify` — build and test this agent's change in a tree of its own.
//!
//! A build in a shared checkout tells you nothing. On 2026-08-16, with four
//! agents in this repository, `cargo check` failed on another agent's
//! half-added `Row::Group` and `cargo test` failed three storyboard tests from
//! a third agent's uncommitted work. A green build there does not mean your
//! change is good and a red one does not mean it is bad — so the isolation
//! worktree in step 3 of `wsp commit-help` is not an extra check, it is the
//! only build whose result means anything.
//!
//! Every agent already does this by hand. They do it differently, they forget
//! parts of it, and the parts they forget are the same three every time:
//!
//! - `env -u GIT_INDEX_FILE` on `git worktree add`, which otherwise writes the
//!   new worktree's index *over* the private one you just built — and leaves
//!   `git apply` refusing an empty patch, which reads like a staging mistake
//!   rather than what it is.
//! - `git read-tree HEAD` *immediately* before the commit rather than minutes
//!   before, so a commit does not land as a silent revert of whatever arrived
//!   in the parent during the build.
//! - The same `env -u` on the `read-tree` that puts the shared index back.
//!
//! This module can only fix the first, because it never commits. It fixes it
//! by construction rather than by remembering: `GIT_INDEX_FILE` is set per
//! command, on the two commands that want it, and stripped from every other —
//! so there is no exported variable for `worktree add` to pick up, whether or
//! not the caller is halfway through the commit procedure with one set.
//!
//! # The scratch tree belongs to the checkout, not to the agent
//!
//! The standing rule from 2026-08-15 was a detached worktree per build, keyed
//! on the workspace and kept, with `CARGO_TARGET_DIR` beside it — one tree per
//! agent to leak rather than one per commit. That argument assumed the agent
//! had nowhere else of its own. Since [`crate::cmd_checkout`] it has: a pane
//! opened on a task stands in `<trunk>/.worktrees/<task>`, where nobody else's
//! half-finished work can appear.
//!
//! Keyed on the workspace, the tree then leaked in the one direction the
//! keying was supposed to stop. A workspace is stable while it exists, which is
//! what made it the right key — but it lasts only as long as the agent in it,
//! and every `wsp spawn` opens a new one. So "one agent, many tasks, one warm
//! tree" was never what happened: measured 2026-08-17,
//! `~/.local/state/wsp/build` held **9.6G in 30 trees**, one per agent that had
//! ever run this command, every one of them cold on its first build and none of
//! them ever removed. A cleanup step nobody runs is the lesson of every leak on
//! robustness-010, and this was one more.
//!
//! So inside a checkout the scratch goes *under the checkout*, at
//! `target/wsp-verify`, and takes its target directory with it. It dies with
//! the tree it belongs to: `wsp checkout --rm` and `--sweep` already remove the
//! whole directory, so there is no second thing to remember and no second thing
//! to leak. No task, no tree — the rule `checkout` states, now true of the
//! build as well. What was unbounded is now one per live task.
//!
//! # Why it does *not* share the checkout's `target/`, which is the obvious idea
//!
//! Because the warmth lives in the target directory and not in the tree, the
//! obvious saving is to point both trees at one `CARGO_TARGET_DIR` — the
//! checkout's own — so that an agent's `cargo test` and this command warm each
//! other and the 295M is not duplicated. It was measured on 2026-08-17 before
//! being believed, and it has to be thrown away.
//!
//! The encouraging half is real: two trees against one target directory do not
//! thrash. The second builds in **0.04s** where the first took 9.7s, and
//! alternating rebuilds only what genuinely differs — `cargo test` 13s against
//! 39s cold.
//!
//! The disqualifying half is that cargo records the dependencies of a unit as
//! *absolute paths*, in `target/debug/.fingerprint/<unit>/dep-*`, and judges
//! freshness by their mtimes. Build the scratch tree and that file comes to
//! read `…/target/wsp-verify/tree/src/cmd_verify.rs`. The next `cargo test` in
//! the **checkout** then asks whether the scratch tree has changed, gets no for
//! an answer, prints `Fresh wsp v0.1.0`, and reruns the old binary. Observed
//! here, not reasoned about: this module was edited, `cargo test` reported 482
//! passing, and the test that had just been added was not among them — the
//! compiled binary still held the previous run's test names.
//!
//! That is a green build for source that was never compiled, in the agent's own
//! tree, arriving silently. It is the exact failure the top of this file
//! describes, reintroduced by the fix for it, and it costs more than the 295M
//! it saves. So the trees get a target directory each, and the saving here is
//! from *bounding* the number of them rather than from sharing one.
//!
//! # A red run is the only run that knows what was red
//!
//! Until 2026-08-18 a failing test run printed this and nothing else:
//!
//! ```text
//! error: test failed, to rerun pass `--bin wsp`
//! ✗ cargo test failed in 25s
//! ```
//!
//! The name was never in it. That is expensive rather than untidy because this
//! suite fails intermittently — five red-then-greens on unmodified trees, the
//! last two at load average ~10 — so the sequence is always: verify goes red,
//! you re-run to find out what broke, the re-run is green, and the name is
//! gone. It cost the name twice in one day. You cannot ask a green run what was
//! red, and `robustness-068` cannot progress without the names.
//!
//! So a failing run keeps three things it used to throw away:
//!
//! - The test path and what its assertion said, parsed out of libtest's own
//!   report and printed by this command rather than left to scrollback.
//! - The whole cargo output, on disk beside `patch.diff`, with the path in the
//!   error line. The tree is already left standing on failure; the run that
//!   produced it is the only one that can be kept.
//! - One re-run of the named test *alone*, and whether it passed there. That
//!   single line — "failed in the suite, passed alone" — is the signature of a
//!   shared-state flake, and taking it on every occurrence is the difference
//!   between a measurement and somebody happening to be watching.
//!
//! What it deliberately does not do is re-run the *suite* and report green. A
//! flake that is automatically swallowed stops being observable, and this one is
//! the most interesting unexplained thing in the repository. The exit status is
//! decided by the run that failed; the re-run alone is evidence printed beside
//! it, never a second opinion that overrules it.
//!
//! # Every test in a process of its own, which is a different instrument
//!
//! `--alone` runs the suite one test per process and prints the tests that
//! failed and nothing else. It is not a slower `cargo test`. It sees a class of
//! bug the ordinary suite cannot see, and on this tree it has now found two:
//!
//! - `robustness-072`: a parent waiting on a file appearing rather than on the
//!   child. Alone, on a quiet machine, **0 passed / 50 failed**.
//! - `robustness-074`: `fake.rs` handing the accepted connection the listener's
//!   `O_NONBLOCK`, so the first `read_line` answered `WouldBlock` and the server
//!   hung up on a client mid-sentence. Alone, **14 failed in 60**.
//!
//! Both were written up as "flaky under load" and both were the opposite:
//! **concurrency was hiding them.** A parent competing with seven hundred other
//! tests gets descheduled after its first poll, which hands the child exactly
//! the milliseconds it needed — so the busier the machine, the greener the
//! suite, and the thing everybody reached for to reproduce a flake was the
//! thing making it go away. Twice is a pattern. The signal is clean: before the
//! `074` fix, two of three passes flagged one test and nothing else; after it,
//! two passes flagged nothing at all.
//!
//! ## What it costs, which is a quarter of what the record says
//!
//! Measured here on 2026-08-19, two builds on the machine:
//!
//! | | 728 tests |
//! |---|---|
//! | `cargo test`, threads, what `verify` runs | **24.6s** |
//! | one process each, warm tree | **85s** |
//! | one process each, counting the cold build | 4m11s |
//!
//! So **3.5× the suite, not the twenty the record implies.** `robustness-074`
//! recorded "701 tests, one process each, about ten minutes", and that number
//! is what made the instrument look like something you schedule rather than
//! something you run. Two things separate it from 85s, and only one of them is
//! method: a `cargo test --exact` per test pays ~48ms of freshness check before
//! libtest starts (measured, five runs), which over 728 tests is ~35s — so
//! running the compiled binary directly accounts for about half a minute of it.
//! The rest is the machine: that pass ran with six agents on this laptop at
//! load 200–350. It is worth knowing which is which, because 85s is a step in
//! a task and ten minutes is not.
//!
//! ## Why it is a flag on this command rather than a script or a `just` target
//!
//! Because the hard part is not the loop, it is the tree. Every property that
//! makes a `verify` result mean anything — a private index at HEAD, a patch of
//! this agent's change and nobody else's, a tree nobody else is standing in —
//! is the same property an alone-pass needs, and a script in the repository
//! would either reinvent all of it or run in the shared checkout and measure
//! whatever four other agents had half-finished. There is no `justfile` here
//! and adding one to hold nine lines would be a second place for the tree rules
//! to drift out of. So the pass reuses the whole of this command up to the
//! build, and adds a loop and a printer.
//!
//! It also inherits the failure parser, which matters more than it sounds: a
//! failure reads identically whether the suite found it or the pass did.
//!
//! ## It does not take a warm tree, and it does not run tests in parallel
//!
//! Two deliberate differences. A warm slot is one of three on this machine and
//! an ordinary build holds it for twenty seconds; this holds a tree for a
//! minute and a half, and cold for four. So `--alone` builds in the agent's own
//! tree — cold the first time, warm after, and dying with the checkout like
//! everything else here. Measured above, that trade is ~2m of extra compile
//! once, against never taking a third of the machine's build capacity away.
//!
//! And it is sequential, which is not laziness: contention between processes is
//! a weaker version of exactly the effect that hid both bugs, and an instrument
//! that reintroduces the thing it exists to remove is a faster way of learning
//! nothing. The 85s is what buys the isolation, and it is the whole product.
//!
//! ## When to run it, which is the harder half
//!
//! Not on an ordinary `verify`. 85s against a 30s verify is not the twenty-fold
//! penalty it was thought to be, but an agent verifies many times in one task and
//! build time is already the scarce resource here (`robustness-077`). Three
//! moments, and one rejected:
//!
//! - **When a test goes red and then green.** The moment both real occasions
//!   actually were, and the one that is mechanised rather than documented: a
//!   red run already re-runs the named test by itself, and when that passes —
//!   the flake signature — the line saying so now names this pass. One line of
//!   output, on red runs only, in front of the only person with a reason to act
//!   on it.
//! - **Before `wsp review`, when the change touched tests.** This is what 85s
//!   buys that ten minutes did not: 85s against a task measured in tens of
//!   minutes is 3%, and it is the only moment that catches an order-dependent
//!   test *on the day it is written* rather than months later by an agent
//!   investigating something else — which is how both of these were found.
//! - **Before a release**, against the trunk, once. Deliberately *not* wired
//!   into `wsp install`: a check on the install path is a check people route
//!   around, and one they choose is worth more than one they resent.
//! - **Not on a schedule.** The machine is almost never idle — six agents was
//!   ordinary while this was written — and a pass that runs at 04:00 reports to
//!   nobody. Ten minutes, or 85 seconds, of output no one owns is the same as
//!   no instrument, which is the failure this whole task is written against. If
//!   it ever does run unattended it has to file a task with the names in it,
//!   and that is a larger thing than this.
//!
//! # The trees are separate; the machine is not
//!
//! A tree per task means a *cold* build per task, and each `cargo` takes `-j8`
//! as though it owned the laptop — load 468 and twenty-one `rustc` on
//! 2026-08-17. So the tree this builds in is no longer always the private one
//! above: [`crate::sharing`] lends it one of a few warm trees kept per
//! repository, exclusively, and hands it a share of the cores. Measured on this
//! command, 40s cold against 19s warm.
//!
//! The isolation is untouched, which is the property that made the warm tree
//! the shareable thing and the target directory not: what arrives is a tree
//! reset to *this* agent's HEAD with *this* agent's patch on it, with nobody
//! else in it while the build runs. Only the tree and its target move; the
//! private index, the patch and the failing run's log stay in this agent's own
//! directory, where two agents cannot overwrite each other's.
//!
//! # Outside a checkout, nothing changes
//!
//! The trunk is still shared — the coordination seat stands there, and so does
//! any bare shell — so an agent building there still gets a tree of its own
//! under `WSP_STATE`, keyed on the workspace, exactly as before. That is where
//! the original argument still holds, and it is the only place left where it
//! does.

use std::path::{Path, PathBuf};
use std::process::Stdio;
use wsp_base::calls::Command;
use std::time::{Duration, Instant};

use serde_json::json;

use crate::sharing::{self, Share};

// ---------------------------------------------------------------- jobs (wsp-054)

/// How verify's runs are run (docs/agent-manager.md 6.2, M3): every one of
/// them (cargo, the test listing and `--alone`'s per-test runs) is a `job.run`
/// on the tree's host, its output echoed from `job.output` and its end read
/// from `job.ended`; nothing is spawned or killed here, and a warm tree's lock
/// names the run, whose liveness is that host's job table. The host is the
/// manager's, or `wsp-host`'s runner in this process under `WSP_JOBS=inproc`
/// and on a host with no manager ([`jobs_host`]).
/// One verify's jobs: the host they go to and the run they belong to.
struct Jobs<'a> {
    host: &'a dyn wsp_wire::host::Host,
    /// The key's row and attempt and the mark every job name ends with
    /// (`verify:<what>:<n>@<mark>`); also what a warm lock names.
    run: sharing::Run,
    /// Jobs sent so far, so each key is unique per run (wsp-040).
    sent: std::sync::atomic::AtomicU32,
}

/// The longest a build may run as a job. `job.run` needs a bound and the
/// direct path has none: this is a backstop against a build that never ends,
/// far beyond any this suite has taken, and not a budget.
const BUILD_TIMEOUT: Duration = Duration::from_secs(4 * 60 * 60);

impl<'a> Jobs<'a> {
    fn new(host: &'a dyn wsp_wire::host::Host, run: sharing::Run) -> Jobs<'a> {
        Jobs { host, run, sent: std::sync::atomic::AtomicU32::new(0) }
    }

    /// The next job's key, `verify:<what>:<n>@<mark>`.
    fn key(&self, what: &str) -> wsp_wire::host::JobKey {
        let n = self.sent.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        wsp_wire::host::job_key(&self.run.row, self.run.attempt, &format!("verify:{what}:{n}@{}", self.run.mark))
    }

    /// The host's job table, as [`sharing::Live::Jobs`] reads it: the rows
    /// of the host this run's jobs go to, `None` for any other host or a table
    /// that could not be read.
    fn table(&self, host: &str) -> Option<Vec<sharing::Seen>> {
        if host != self.run.host {
            return None;
        }
        let rows = self.host.jobs().ok()?;
        Some(
            rows.into_iter()
                .map(|r| sharing::Seen {
                    row: r.key.row.0,
                    attempt: r.key.attempt,
                    job: r.key.job,
                    started: r.started.0,
                    running: r.state == wsp_wire::host::JobState::Running,
                })
                .collect(),
        )
    }

    /// Run `argv` as one job in `dir`, its env prefix `env`, and answer its
    /// end and the whole of what it wrote. `seen` is handed each `job.output`
    /// as it arrives, which is what the two pump threads did for a child.
    #[allow(clippy::too_many_arguments)]
    fn run(
        &self,
        what: &str,
        class: wsp_wire::host::JobClass,
        dir: &Path,
        env: &[(String, String)],
        argv: Vec<String>,
        timeout: Duration,
        seen: &mut dyn FnMut(&str),
    ) -> Result<(wsp_wire::host::JobEnded, String), String> {
        // The place comes from `place_of`, never from a path joined here.
        let at = place_of(Whose::Row(&self.run.row), TreeKind::Verify, dir)?.at;
        // `env -u K -- K=V … cmd`: the values wsp mints cross on the argv
        // (host-api `JobRun`), and the caller's private index does not.
        let mut full: Vec<String> = vec!["env".into(), "-u".into(), "GIT_INDEX_FILE".into(), "--".into()];
        full.extend(env.iter().map(|(k, v)| format!("{k}={v}")));
        full.extend(argv);
        let run = wsp_wire::host::JobRun {
            key: self.key(what),
            class,
            argv: full,
            at,
            timeout_secs: timeout.as_secs().max(1),
            hung_after_secs: None,
            detach: false,
        };
        let mut text = String::new();
        let mut sink = |o: &wsp_wire::host::JobOutput| {
            text.push_str(&o.text);
            seen(&o.text);
        };
        let ended = self.host.run_job(&run, &mut sink).map_err(|e| format!("{e:?}"))?;
        // The log is the whole of it; `job.output` is a view a slow reader can
        // have gaps in. It is on this host, so it is read when it is there.
        let log = std::fs::read_to_string(&ended.log.path).ok().filter(|l| l.len() >= text.len());
        Ok((ended, log.unwrap_or(text)))
    }
}

// Where a verify job runs: `place_of(row, verify)`, the one function a check,
// a verifier and a barrier take a job's place from (wsp-053). It answers a
// `HostDir` on the row's host relative to that host's tree root, never a
// path on wsp's machine.
use crate::job_place::{host_of, place_of, TreeKind, Whose};

/// `to` as seen from `from`, both on the jobs' host: how a job names its
/// target directory and a test binary without a path on wsp's machine.
fn relative(from: &Path, to: &Path) -> PathBuf {
    let canon = |p: &Path| p.canonicalize().unwrap_or_else(|_| p.to_path_buf());
    let (from, to) = (canon(from), canon(to));
    let a: Vec<_> = from.components().collect();
    let b: Vec<_> = to.components().collect();
    let common = a.iter().zip(&b).take_while(|(x, y)| x == y).count();
    let mut out = PathBuf::new();
    for _ in common..a.len() {
        out.push("..");
    }
    for c in &b[common..] {
        out.push(c.as_os_str());
    }
    if out.as_os_str().is_empty() {
        out.push(".");
    }
    out
}

/// The run this verify is, under `manager`: its row is the checkout's when it
/// stands in one, else [`agent_key`]'s workspace, standing in for a row as a
/// barrier's list and group do (the governor's answer, 2026-11-20); the mark
/// is the start in milliseconds and this pid, so each run's keys are its own
/// (wsp-051).
///
/// Its host is the row's, as `place_of` names it ([`host_of`]), so a warm
/// lock is read from the job table of the host its jobs run on.
fn this_run(checkout: Option<&Path>, key: &str) -> sharing::Run {
    let row = checkout
        .and_then(|c| c.file_name())
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_else(|| key.to_string());
    let ms = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0, |d| d.as_millis());
    let host = host_of(Whose::Row(&row));
    sharing::Run { host, row, attempt: 0, mark: format!("{ms}-{}", std::process::id()) }
}

/// The host `run`'s jobs go to: the manager's under `WSP_JOBS=manager` when
/// the run's host is the host this machine's manager is (6.2: while the Mac's
/// manager is the only one, a row on a host with no manager runs in this
/// process), else `wsp-host`'s runner in this process. An error is the choice
/// or the manager failing, said once, and nothing is run in this process in
/// its place.
fn jobs_host(state: &Path, run: &sharing::Run) -> Result<&'static dyn wsp_wire::host::Host, String> {
    use wsp_wire::host::{choose_jobs, JobsChoice};
    let version = env!("CARGO_PKG_VERSION");
    match JobsChoice::from_env().map_err(|e| e.to_string())? {
        JobsChoice::Inproc => choose_jobs(JobsChoice::Inproc, state, version).map_err(|e| e.to_string()),
        JobsChoice::Manager => {
            let ours = wsp_wire::manager::read_record(state).map(|(local, _)| local.host);
            if ours.as_deref() != Some(run.host.as_str()) {
                return choose_jobs(JobsChoice::Inproc, state, version).map_err(|e| e.to_string());
            }
            wsp_wire::host::jobs_chosen(state, version).map_err(|e| e.to_string())
        }
    }
}

impl Jobs<'_> {
    /// How a warm lock's holder is asked whether it is still there: by the
    /// job table of the host the run's jobs go to.
    fn live<'s>(&'s self, table: &'s dyn Fn(&str) -> Option<Vec<sharing::Seen>>) -> sharing::Live<'s> {
        sharing::Live::Jobs { me: &self.run, table }
    }
}
use crate::store::Store;
use crate::util;
use crate::base::args::Args;

pub use crate::base::git::{git, git_ok, toplevel};


/// Which agent is asking.
///
/// The workspace, not the pane, and the reason first written here was wrong. It
/// is not that pane ids are reissued — inside a workspace that survives they are
/// not (`robustness-084`). It is that an agent is a workspace and not a pane: it
/// splits, it opens a shell beside itself, and `wsp verify` run from any of them
/// is the same agent asking for the same tree. Keyed on the pane, every split
/// would be a cold build. Outside backend there is no workspace, and `solo` is
/// honest: one shell at a terminal gets one tree and shares it with the next.
///
/// A reissued workspace id therefore inherits the previous occupant's warm tree,
/// and that is fine here and nowhere else: a build tree is a cache of a git
/// state, so the worst it costs is a rebuild. It is named because the same
/// inheritance in a claim is `robustness-089`.
/// `wsp sandbox` keys its instance the same way, deliberately: an agent's build
/// tree and its sandbox are the same pair of scratch things, and one name for
/// both means `verify` and `sandbox` cannot disagree about whose they are.
pub fn agent_key() -> String {
    if let Ok(v) = std::env::var("WSP_AGENT") {
        if !v.trim().is_empty() {
            return util::slugify(v.trim());
        }
    }
    "solo".to_string()
}

/// Where an agent standing in the *trunk* keeps its build tree: under the state
/// directory rather than the store, because it is machine-local, it is large,
/// and it is not worth committing. Under `WSP_STATE` rather than a fixed path
/// so a sandbox (see robustness-013) gets its own and does not warm — or corrupt
/// — the real one.
///
/// Named for the repository and keyed on the agent. `repo` has to be the trunk
/// for the name to mean anything, which is why [`scratch`] resolves it rather
/// than passing whatever tree the caller is standing in.
pub fn build_dir(store: &Store, repo: &Path, key: &str) -> PathBuf {
    let name = repo.file_name().and_then(|s| s.to_str()).unwrap_or("repo");
    store.state.join("build").join(format!("{}-{}", util::slugify(name), key))
}

/// The keys a build tree may still be named for: the seats compound holds, and
/// the `solo` key of a shell that is no seat at all.
///
/// `None` is compound not answering, which is the guard [`clear_build_dirs`]
/// describes: a census that was not taken is not a machine with nothing on it.
fn live_keys() -> Option<Vec<String>> {
    use crate::place::Place;
    let census = crate::place_compound::Compound::new().census().ok()?;
    keys_of(&census)
}

/// [`live_keys`] off a census already taken, so a test can stand seats without
/// a compound.
fn keys_of(census: &crate::place::Census) -> Option<Vec<String>> {
    if !census.answered("") {
        return None;
    }
    let mut keys: Vec<String> = census.seats().map(|s| s.seat.as_str().to_string()).collect();
    keys.push("solo".to_string());
    Some(keys)
}

/// Remove the build trees under the state directory that no live seat
/// owns, and say which went.
///
/// The residue of keying on the workspace. Those trees outlived the workspaces
/// that named them, so no agent alive can name one to remove it — 9.6G in 30 of
/// them on 2026-08-17 — and the arrangement that made them is gone.
///
/// `live` is the workspace ids backend reported, and `None` is backend not
/// answering. That distinction is the whole guard, and it is the same one
/// [`crate::cmd_agent::may_reap`] makes for the same reason: silence is not
/// evidence that the work stopped. A backend that is down, or slow, reports
/// nothing, which looks exactly like a machine with no agents on it — and this
/// would then delete the tree every running agent is mid-build in. So `None`
/// removes nothing at all, and an empty list is only believed when backend said
/// it.
///
/// Passed in rather than read here so the judgement can be tested without a
/// live backend, which is the one thing that would make it untestable and it is
/// the only thing here worth testing.
///
/// Directories only. A stale `git worktree` registration left behind is pruned
/// by [`ensure_tree`] or by `checkout` the next time either touches the
/// repository, and pruning it here would mean guessing which repository each
/// tree came from.
///
/// # Two functions, because `-n` has to ask the same question
///
/// The judgement is [`stale_build_dirs`] and this is the removal of what it
/// picked. Split for `worklist-051`: a preview computed *beside* the act is a
/// second opinion about a directory listing crossed with a live workspace list,
/// and the one run where the two would disagree is the run where backend changed
/// its answer between them. Asking once and removing that answer is the only
/// arrangement where the preview cannot be wrong about the act.
fn clear_build_dirs(store: &Store, live: Option<&[String]>, mine: &Path) -> Vec<String> {
    let mut gone = Vec::new();
    for path in stale_build_dirs(store, live, mine) {
        if std::fs::remove_dir_all(&path).is_ok() {
            gone.push(util::contract(&path));
        }
    }
    gone.sort();
    gone
}

/// The build trees under the state directory no live workspace owns.
///
/// The judgement half of [`clear_build_dirs`], and the whole of what
/// `wsp verify --rm --all -n` has to say about the residue. Sorted, because the
/// preview and the act both print it and a directory listing arrives in
/// whatever order the filesystem felt like.
///
/// `None` for `live` is backend not answering, and it is the guard rather than an
/// error path: see [`clear_build_dirs`] above. It matters twice as much to the
/// preview, because "backend is silent, so nothing here would go" and "there is
/// no residue" are the same empty list and very much not the same fact — which
/// is why the caller says which one it got rather than printing a bare zero.
fn stale_build_dirs(store: &Store, live: Option<&[String]>, mine: &Path) -> Vec<PathBuf> {
    let Some(live) = live else { return Vec::new() };
    let root = store.state.join("build");
    let Ok(entries) = std::fs::read_dir(&root) else { return Vec::new() };
    let mut stale = Vec::new();
    for e in entries.flatten() {
        let path = e.path();
        if !path.is_dir() || path == mine {
            continue;
        }
        let Some(name) = path.file_name().and_then(|s| s.to_str()) else { continue };
        if live.iter().any(|ws| keyed_on(name, ws)) {
            continue;
        }
        stale.push(path);
    }
    stale.sort();
    stale
}

/// Whether a build tree under `build/` belongs to this workspace.
///
/// `<repo>-<workspace>`, and the repository name can hold a dash, so the key is
/// the tail rather than the second field.
fn keyed_on(name: &str, workspace: &str) -> bool {
    name.ends_with(&format!("-{}", util::slugify(workspace)))
}

/// Remove the build trees keyed on one workspace, and say which went.
///
/// [`clear_build_dirs`] with the question turned around, and the difference is
/// the guard rather than the mechanism. That one asks which workspaces are
/// alive and removes everything else, which is right for a sweep and wrong for
/// a single ending: it would make `wsp despawn` reach every dead workspace's
/// residue on a list that is only as good as backend's last answer. This one is
/// told the workspace whose last pane has just been closed, so the only tree it
/// can touch is one that provably has no owner left.
///
/// Directories only, for [`clear_build_dirs`]'s reason: the stale `git
/// worktree` registration is pruned by [`ensure_tree`] or `checkout` the next
/// time either touches the repository, and pruning it here would mean guessing
/// which repository the tree came from.
pub fn clear_build_key(store: &Store, workspace: &str) -> Vec<String> {
    let Ok(entries) = std::fs::read_dir(store.state.join("build")) else { return Vec::new() };
    let mut gone = Vec::new();
    for e in entries.flatten() {
        let path = e.path();
        let Some(name) = path.file_name().and_then(|s| s.to_str()) else { continue };
        if !path.is_dir() || !keyed_on(name, workspace) {
            continue;
        }
        if std::fs::remove_dir_all(&path).is_ok() {
            gone.push(util::contract(&path));
        }
    }
    gone.sort();
    gone
}

/// Everything one run of `verify` needs a place for.
///
/// Resolved once and passed around, because the two arrangements differ in more
/// than one path and a caller that worked out three of them separately would be
/// able to get two right and one wrong. `wsp install` looks for the release
/// build this produced, and used to compute the directory itself — from the
/// tree it was standing in rather than the trunk, so inside a checkout it
/// looked somewhere `verify` had never written.
pub struct Scratch {
    /// Holds the private index, the patch, and `tree`. What `--rm` removes.
    pub dir: PathBuf,
    /// The source tree that gets built: HEAD, reset and re-patched every run.
    pub tree: PathBuf,
    /// `CARGO_TARGET_DIR`, always inside `dir` and never shared with the tree
    /// the agent edits in — see the header for the measurement that settled
    /// that, and what sharing it silently did to `cargo test`.
    pub target: PathBuf,
    /// The checkout it belongs to, or `None` for the trunk. The caller wants
    /// the distinction to say which arrangement it is looking at.
    pub checkout: Option<PathBuf>,
}

/// Which of the two arrangements applies, and where each part goes.
///
/// The test is the path rule and not a git question: a per-task checkout is
/// exactly a tree whose own root is the `<trunk>/.worktrees/<task>` that
/// [`crate::cmd_checkout::worktree_of`] names. Asking git which worktree owns a
/// path costs a process, and the layout is ours to define, so the answer is
/// already in the path — the same bargain `overlap` makes for the same reason.
pub fn scratch(store: &Store, repo: &Path, key: &str) -> Scratch {
    if crate::cmd_checkout::worktree_of(repo).as_deref() == Some(repo) {
        // Under `target/` rather than a dotted directory of its own: it is
        // build output, it is already gitignored in every cargo project, and
        // putting it there keeps `git status` in the checkout clean — which
        // matters more than tidiness, because `checkout --sweep` refuses to
        // remove a tree with anything uncommitted in it, and a scratch
        // directory git could see would make every tree permanently unsweepable.
        let dir = repo.join("target").join("wsp-verify");
        return Scratch {
            tree: dir.join("tree"),
            target: dir.join("target"),
            dir,
            checkout: Some(repo.to_path_buf()),
        };
    }
    let named_for = crate::cmd_checkout::trunk(repo).unwrap_or_else(|| repo.to_path_buf());
    let dir = build_dir(store, &named_for, key);
    Scratch { tree: dir.join("tree"), target: dir.join("target"), dir, checkout: None }
}

/// The file naming where the last build here actually put its artefacts.
///
/// One line, in the agent's own scratch directory. It exists because the answer
/// stopped being derivable: a build now lands in whichever warm tree was free,
/// and `wsp install` — which looks for the release binary this command produced
/// — cannot work that out from the checkout it is standing in.
pub const BUILT_AT: &str = "built-at";

/// What the last build here wrote down, or `None` if none has — or if it names
/// a tree that has since gone, which `--rm` makes ordinary rather than exotic.
///
/// Named rather than read twice, because `install` asks the same question for a
/// second reason: whether it may name a path in "nothing built to install". A
/// build lands in whichever warm tree was free, so the only path anybody can
/// honestly name is the one a build wrote down.
pub fn built_at(dir: &Path) -> Option<PathBuf> {
    std::fs::read_to_string(dir.join(BUILT_AT))
        .ok()
        .map(|s| PathBuf::from(s.trim()))
        .filter(|p| p.is_dir())
}

/// Where the last build here put its target directory, for a caller that wants
/// the artefacts rather than a place to build.
///
/// Falls back to [`scratch`] when there is no pointer or it names something
/// gone, which is both the pre-warm-tree arrangement and the honest answer for
/// a checkout that has never run a build.
pub fn last_build(store: &Store, repo: &Path, key: &str) -> Scratch {
    let sc = scratch(store, repo, key);
    match built_at(&sc.dir) {
        Some(target) => {
            let tree = target.parent().map_or_else(|| sc.tree.clone(), |d| d.join("tree"));
            Scratch { tree, target, ..sc }
        }
        None => sc,
    }
}

/// Remove warm trees: the one this agent last built in, or with `--all` every
/// one for this repository that nobody is building in.
///
/// The default is the narrow one deliberately. `--rm` means "the tree I have
/// been building in has gone wrong", and after this change that tree is shared
/// — so throwing away all three on the way past would hand every other agent a
/// cold build to fix one agent's problem. Which one it was is not a guess: the
/// build wrote it down.
///
/// A tree somebody is building in is never removed, and the git worktree
/// registration goes with the directory. A directory removed without it makes
/// the next `worktree add` refuse, with a message that reads like a bug in this
/// command rather than a stale registration.
fn clear_warm(store: &Store, live: sharing::Live, repo: &Path, named_for: &Path, dir: &Path, all: bool) -> usize {
    let mut gone = 0;
    for w in free_warm(store, live, named_for, dir, all) {
        let _ = git(repo, &["worktree", "remove", "--force", &w.tree().display().to_string()]);
        if std::fs::remove_dir_all(&w.dir).is_ok() {
            gone += 1;
        }
        let _ = git(repo, &["worktree", "prune"]);
    }
    gone
}

/// The warm trees this call would take: exactly the ones [`clear_warm`] then
/// removes, held while the caller looks at them.
///
/// # Why the preview takes the locks
///
/// A dry run that answered "which trees are free" by *reading* the lock files
/// would be a second implementation of [`crate::sharing::claim_at`], and it
/// would differ from it on the case that arrangement was built for: a lock left
/// by a build that was killed reads as held and is in fact free, because the
/// claim reclaims it. So the preview asks the same question the same way, and
/// what it gets back is the answer, not an estimate of it.
///
/// The cost is that a dry run writes — a lock file per free slot, dropped when
/// the returned `Warm`s do. That is the same write every build makes on its way
/// in, held for as long as it takes to print a line instead of as long as a
/// build; and there is no way to be truthful here without it. `-n` promises not
/// to remove anything, which this keeps.
///
/// The `dir.exists()` filter belongs here rather than in the removal: a claim
/// on a slot nothing has built in yet succeeds and hands back a directory that
/// is not there ([`crate::sharing::claim_at`] makes the lock, not the tree), so
/// a preview without it would name three trees on a machine that has one.
fn free_warm(store: &Store, live: sharing::Live, named_for: &Path, dir: &Path, all: bool) -> Vec<crate::sharing::Warm> {
    let claimed = if all {
        crate::sharing::warm_each(&store.state, named_for, crate::sharing::WARM_TREES, live)
    } else {
        // Which one it was is not a guess: the build wrote it down.
        std::fs::read_to_string(dir.join(BUILT_AT))
            .ok()
            .map(|s| PathBuf::from(s.trim()))
            .and_then(|target| target.parent().map(Path::to_path_buf))
            .filter(|d| d.starts_with(store.state.join("warm")))
            .and_then(|d| crate::sharing::warm_named(&d, live))
            .into_iter()
            .collect()
    };
    claimed.into_iter().filter(|w| w.dir.exists()).collect()
}

/// The paths whose change is under test, as `git add` pathspecs.
///
/// Naming them is the point — "apply the diff for the paths you name, and
/// nothing else" — but requiring them on every call would make the command
/// something people skip. So the default is everything changed, and the file
/// list is printed either way, prominently, because that is step 2 of the
/// commit procedure: a wrong file list is the tell, and the hunks inside one
/// all look plausible because they are somebody's real work.
fn staged_files(index: &Path, repo: &Path) -> Vec<String> {
    let out = Command::new("git")
        .arg("-C")
        .arg(repo)
        .args(["diff", "--cached", "--name-only"])
        .env("GIT_INDEX_FILE", index)
        .stderr(Stdio::null())
        .output()
        .ok();
    out.map(|o| {
        String::from_utf8_lossy(&o.stdout)
            .lines()
            .map(|s| s.trim().to_string())
            .filter(|s| !s.is_empty())
            .collect()
    })
    .unwrap_or_default()
}

/// Everything the repository has that HEAD does not, named or not.
///
/// The counterweight to naming paths. Naming them keeps another agent's work
/// out of your build, which is the point — but it also lets you leave out
/// something of *your own*, and that fails in the worst direction: a patch
/// holding a new module that nothing declares compiles perfectly, because
/// nothing compiles it. Measured while writing this, `wsp verify
/// src/cmd_verify.rs` went green in 7s against a change that did not build.
///
/// So what was left out is printed beside what was taken. In a shared tree
/// most of it will be somebody else's and correctly excluded; the whole value
/// is that you can see at a glance whether one of them is yours.
fn changed_files(repo: &Path) -> Vec<String> {
    let Some(out) = git(repo, &["status", "--porcelain", "--untracked-files=all"]) else {
        return Vec::new();
    };
    out.lines()
        .filter_map(|l| l.get(3..))
        // A rename is written `old -> new`; the new name is the one on disk.
        .map(|p| p.rsplit(" -> ").next().unwrap_or(p).trim().trim_matches('"').to_string())
        .filter(|p| !p.is_empty())
        .collect()
}

/// Build the patch under test, through a private index at HEAD.
///
/// The same three commands as steps 1 and 3 of `wsp commit-help`, in the same
/// order, which is deliberate: what this builds is exactly what that procedure
/// would stage, so a green `verify` is a statement about the commit you are
/// about to make rather than about some neighbouring tree.
///
/// `git add` rather than `git diff HEAD` because `add` sees files that are not
/// tracked yet. A new module is the commonest thing an agent adds and the
/// easiest thing for a diff-based patch to miss — and missing it fails in the
/// worst direction, with a green build for a change that does not compile.
/// `scratch` is this command's own working directory — the private index, the
/// patch, the build tree. Inside a checkout it is under the tree by design, and
/// a sandbox pointed at a state directory in the tree puts it there by accident;
/// either way `git add -A` would otherwise stage verify's own index into the
/// patch it is building, which is a change that then fails to apply against HEAD
/// for reasons no one could read.
///
/// The exclusion is asked for only when git cannot already see it, and that is
/// not an optimisation. An `:(exclude)` pathspec under an ignored directory
/// reads to `git add` as an ignored path named on purpose, so it refuses the
/// whole command — `error: the following paths are ignored ... target`. The
/// checkout arrangement puts the scratch under `target/`, which every cargo
/// project ignores, so the guard against staging it is exactly what made it
/// impossible to stage anything.
fn build_patch(
    repo: &Path,
    cwd: &Path,
    index: &Path,
    scratch: &Path,
    paths: &[String],
) -> Result<String, String> {
    let _ = std::fs::remove_file(index);
    let run = |dir: &Path, args: &[&str]| -> Result<std::process::Output, String> {
        Command::new("git")
            .arg("-C")
            .arg(dir)
            .args(args)
            .env("GIT_INDEX_FILE", index)
            .output()
            .map_err(|e| format!("git: {e}"))
    };
    let check = |out: std::process::Output, what: &str| -> Result<Vec<u8>, String> {
        if out.status.success() {
            return Ok(out.stdout);
        }
        let msg = String::from_utf8_lossy(&out.stderr).trim().to_string();
        Err(if msg.is_empty() { format!("{what} failed") } else { msg })
    };

    check(run(repo, &["read-tree", "HEAD"])?, "read-tree")?;

    // Pathspecs are resolved from where they were typed, so the named case runs
    // in the caller's directory rather than at the repository root.
    if paths.is_empty() {
        let mut argv: Vec<String> = vec!["add".into(), "-A".into(), "--".into(), ".".into()];
        if let Ok(rel) = scratch.strip_prefix(repo) {
            let rel = rel.display().to_string();
            // `check-ignore` exits 0 when the path is ignored, which is `git`
            // returning `Some` here.
            if git(repo, &["check-ignore", "-q", "--", &rel]).is_none() {
                argv.push(format!(":(exclude){rel}"));
            }
        }
        let argv: Vec<&str> = argv.iter().map(|s| s.as_str()).collect();
        check(run(repo, &argv)?, "add")?;
    } else {
        let mut argv: Vec<&str> = vec!["add", "--"];
        argv.extend(paths.iter().map(|s| s.as_str()));
        check(run(cwd, &argv)?, "add")?;
    }

    let patch = check(run(repo, &["diff", "--cached", "--binary"])?, "diff")?;
    Ok(String::from_utf8_lossy(&patch).into_owned())
}

/// Make this agent's worktree if it has none, and put it back to `head` if it
/// has. `--detach` because the tree holds one agent's patch and never a branch:
/// there is nothing here to push, and a branch would be a second thing to keep
/// true.
fn ensure_tree(repo: &Path, tree: &Path, head: &str) -> Result<bool, String> {
    if tree.join(".git").exists() {
        // `reset` puts tracked files back; `clean` removes what the last run's
        // patch added, which reset leaves behind as untracked. Both are safe
        // here in a way they would never be in a shared checkout — nothing in
        // this tree is anybody's working copy.
        git_ok(tree, &["reset", "--hard", head, "--quiet"])?;
        git_ok(tree, &["clean", "-fdq"])?;
        return Ok(false);
    }
    // A directory left behind by a removed worktree, or a worktree registration
    // whose directory is gone: both make `add` refuse, and both are the leak
    // this command exists to stop leaving.
    let _ = std::fs::remove_dir_all(tree);
    let _ = git(repo, &["worktree", "prune"]);
    std::fs::create_dir_all(tree.parent().unwrap_or(tree)).map_err(|e| e.to_string())?;
    git_ok(repo, &["worktree", "add", "--detach", "--quiet", &tree.display().to_string(), head])?;
    Ok(true)
}

/// One cargo run: echoed as it arrives, and kept.
///
/// It is echoed because the compiler's own output is the thing being asked for
/// — a summary of a build failure is worth nothing next to the error — and
/// `echo` is off only for `--json`, where the caller wants one object rather
/// than a build log with an object at the end of it.
///
/// It is kept unconditionally, which it was not before: the run that fails is
/// the only run that knows what failed, and on this suite the next one is
/// usually green. Nothing here decides that a run is interesting; by the time
/// anything could, the output would be gone.
///
/// One `job.run` of class `build` in the tree's place. `CARGO_TARGET_DIR`
/// (relative to the tree, so no path on wsp's machine crosses) and the share's `CARGO_BUILD_JOBS` cross as the env
/// prefix, and the output is echoed from `job.output` as it arrives.
fn cargo_job(jobs: &Jobs, tree: &Path, target: &Path, argv: &[&str], echo: bool, share: &Share) -> (bool, String) {
    let (k, v) = share.env();
    let env = vec![
        ("CARGO_TARGET_DIR".to_string(), relative(tree, target).display().to_string()),
        (k.to_string(), v),
    ];
    let mut full = vec!["cargo".to_string()];
    full.extend(argv.iter().map(|a| a.to_string()));
    let what = argv.first().copied().unwrap_or("cargo");
    let mut seen = |text: &str| {
        use std::io::Write;
        if echo {
            // Flushed per chunk, as the pumps did: progress is only progress
            // if it arrives while the build is still running.
            let mut h = std::io::stdout();
            let _ = h.write_all(text.as_bytes());
            let _ = h.flush();
        }
    };
    match jobs.run(&format!("cargo-{what}"), wsp_wire::host::JobClass::Build, tree, &env, full, BUILD_TIMEOUT, &mut seen) {
        Ok((ended, text)) => (ended.exit == Some(0), text),
        Err(e) => {
            if echo {
                eprintln!("wsp: cargo: {e}");
            }
            (false, format!("cargo: {e}"))
        }
    }
}

/// One test the run reported as failed: where it panicked, and what it said.
#[derive(Debug, Clone, PartialEq)]
pub struct Failure {
    pub name: String,
    pub at: Option<String>,
    pub message: Option<String>,
}

/// The failing tests, out of a cargo test run's own report.
///
/// Two things in libtest's output name a failure and neither is enough alone.
/// The trailing `failures:` list is the authoritative set — every failed test
/// is in it, in one place, whatever the format — but it carries no message. The
/// `---- <name> stdout ----` blocks above it carry the panic and the assertion
/// but are the part a harness is free to shorten. So the list decides who
/// failed, the blocks say what they said, and a name in the list with no block
/// is still reported by name.
///
/// A compile failure names nothing here, deliberately. Cargo has already
/// printed the error, in full, and inventing a test name for it would be worse
/// than the silence this replaces.
fn failures(output: &str) -> Vec<Failure> {
    let lines: Vec<&str> = output.lines().collect();

    let mut detail: Vec<Failure> = Vec::new();
    let mut i = 0;
    while i < lines.len() {
        let head = lines[i].trim();
        let Some(name) = head.strip_prefix("---- ").and_then(|r| r.strip_suffix(" stdout ----"))
        else {
            i += 1;
            continue;
        };
        let mut block: Vec<&str> = Vec::new();
        let mut j = i + 1;
        while j < lines.len() {
            let l = lines[j].trim();
            if l.starts_with("---- ") || l == "failures:" || l.starts_with("test result:") {
                break;
            }
            block.push(lines[j]);
            j += 1;
        }
        detail.push(Failure {
            name: name.trim().to_string(),
            at: panic_site(&block),
            message: assertion(&block),
        });
        i = j;
    }

    let mut names: Vec<String> = Vec::new();
    let mut k = 0;
    while k < lines.len() {
        if lines[k].trim() != "failures:" {
            k += 1;
            continue;
        }
        k += 1;
        // The detail section also opens with `failures:`, and what follows it
        // is a blank line and an unindented `----` header — so the indent test
        // ends that one immediately and only the real list is collected.
        while k < lines.len() && lines[k].starts_with("    ") && !lines[k].trim().is_empty() {
            let n = lines[k].trim().to_string();
            if !names.contains(&n) {
                names.push(n);
            }
            k += 1;
        }
    }

    if names.is_empty() {
        return detail;
    }
    names
        .into_iter()
        .map(|n| match detail.iter().find(|f| f.name == n) {
            Some(f) => f.clone(),
            None => Failure { name: n, at: None, message: None },
        })
        .collect()
}

/// `src/panel/keys.rs:412:9`, out of the panic line.
fn panic_site(block: &[&str]) -> Option<String> {
    let line = block.iter().find(|l| l.contains("panicked at "))?;
    let rest = line.split("panicked at ").nth(1)?.trim().trim_end_matches(':');
    (!rest.is_empty()).then(|| rest.to_string())
}

/// What the assertion said — the line under the panic, or failing that the
/// first thing the test printed, which is where a `Result`-returning test's
/// error ends up.
fn assertion(block: &[&str]) -> Option<String> {
    let after = block.iter().position(|l| l.contains("panicked at ")).map(|p| p + 1).unwrap_or(0);
    block[after.min(block.len())..]
        .iter()
        .map(|l| l.trim())
        .find(|l| !l.is_empty() && !l.starts_with("note: "))
        .map(|l| l.to_string())
}

/// Run one named test by itself, once, and say whether it passed there.
///
/// `Some(true)` is the whole signature of a shared-state flake and the
/// measurement `robustness-068` wants on every occurrence. It is evidence
/// printed beside the failure and never a verdict: the suite failed, and the
/// exit status is still the suite's.
///
/// `None` is "not measured", which is not the same as passing and is printed as
/// nothing at all rather than as a green word.
fn rerun_alone(runs: &Jobs, tree: &Path, target: &Path, name: &str, share: &Share) -> Option<bool> {
    // A doc test is named `src/store.rs - Store::at (line 12)`, which is not
    // something libtest can be handed as an exact filter. Anything with a space
    // in it is left alone rather than guessed at.
    if name.is_empty() || name.split_whitespace().count() != 1 {
        return None;
    }
    let (ok, text) = cargo_job(runs, tree, target, &["test", "--workspace", "--quiet", "--", "--exact", name], false, share);
    if !ok {
        return Some(false);
    }
    // A filter that matched nothing exits green. Reporting that as "passed
    // alone" would invent the measurement instead of taking it, so the pass has
    // to be one test actually having run.
    text.contains("1 passed; 0 failed").then_some(true)
}

/// How long one test alone may take before the pass calls it a failure.
///
/// A deadlock is exactly the class of bug this pass hunts — `robustness-072`
/// was a wait on a file that never appeared, and the fix was to wait on the
/// child instead — so a test that hangs is a result and not an accident. With
/// no limit the first one to hang costs the whole pass and prints nothing at
/// the end of it, which is the one failure mode an instrument nobody is
/// watching cannot have. That is the whole job: bound a hang. It is not a
/// statement about how long a test ought to take.
///
/// Which is why the number is enormous. Measured 2026-08-19 over all 776
/// tests, one process each: median 10ms, p99 550ms, and the slowest in the
/// suite 2.31s. With six competing processes on eight cores — roughly what
/// this laptop looks like with other agents on it — that slowest test took
/// 6.77s, an inflation of 2.9x. Five minutes clears it by about 44x.
///
/// The margin is that wide because the tail grows faster than the median does,
/// and because being wrong in the two directions costs wildly different
/// things. This constant's smaller sibling proved both. `run_alone`'s own test
/// held a *do-nothing* child — `#!/bin/sh` and `exit 0`, observed done in 6ms
/// on a quiet machine — to five seconds, and on a loaded one it missed that
/// budget in 11 of 150 runs. A budget only a small multiple above the observed worst
/// case is a budget that reports the machine. And when it does, the instrument
/// says *this test does not survive alone* about a test that does, which is
/// how an instrument stops being believed — whereas a budget that is too
/// generous costs one genuinely hung test five minutes, once, in a pass that
/// prints each failure as it happens rather than at the end.
const ALONE_TIMEOUT: Duration = Duration::from_secs(300);

/// The compiled test binaries, out of cargo's own JSON.
///
/// `--no-run` rather than a `cargo test` per test: cargo spends ~48ms deciding
/// nothing has changed before libtest starts (measured over five runs), which
/// across 728 tests is ~35s of the pass spent asking a question whose answer
/// cannot have moved. The point is one *test* process each, not one *cargo*
/// each. What comes back is the executable itself, which the pass runs directly.
///
/// The JSON is on stdout and everything a human reads is on stderr, so the two
/// halves separate on the leading brace — which is also how a compile failure
/// gets reported here without printing a build log made of objects.
fn test_binaries(runs: &Jobs, tree: &Path, target: &Path, share: &Share) -> Result<Vec<PathBuf>, String> {
    let argv = &["test", "--workspace", "--no-run", "--message-format=json"];
    let (ok, text) = cargo_job(runs, tree, target, argv, false, share);
    if !ok {
        let msg: Vec<&str> = text.lines().filter(|l| !l.starts_with('{')).collect();
        return Err(msg.join("\n"));
    }
    let exes = executables(&text);
    if exes.is_empty() {
        return Err("cargo built no test binaries".to_string());
    }
    Ok(exes)
}

/// The test binaries in a `--message-format=json` run, and nothing else in it.
///
/// `profile.test` rather than the target kind: a build script and the crate's
/// own `lib` both come past as artifacts, and only the ones compiled with the
/// test harness can be handed a test name.
fn executables(text: &str) -> Vec<PathBuf> {
    let mut exes = Vec::new();
    for line in text.lines().filter(|l| l.starts_with('{')) {
        let Ok(v) = serde_json::from_str::<serde_json::Value>(line) else { continue };
        if v["reason"] != "compiler-artifact" || v["profile"]["test"] != true {
            continue;
        }
        if let Some(e) = v["executable"].as_str() {
            exes.push(PathBuf::from(e));
        }
    }
    exes
}

/// Every test in one binary, asked of the binary rather than guessed.
///
/// Doc tests are not here and are not missing: rustdoc compiles and runs each
/// one as its own process already, so they are the one part of `cargo test`
/// that this pass would have nothing to add to.
///
/// Under `WSP_JOBS=manager` the listing is a job in the tree's place, bounded
/// by [`ALONE_TIMEOUT`] as a per-test run is: a binary that never answers its
/// `--list` is ended by the manager and lists nothing.
fn test_names(runs: &Jobs, tree: &Path, exe: &Path) -> Vec<String> {
    let argv = vec![relative(tree, exe).display().to_string(), "--list".into(), "--format".into(), "terse".into()];
    match runs.run("list", wsp_wire::host::JobClass::Check, tree, &[], argv, ALONE_TIMEOUT, &mut |_| {}) {
        Ok((ended, text)) if ended.exit == Some(0) => listed(&text),
        _ => Vec::new(),
    }
}

/// `name: test` per line, and a trailing count line that is not one.
///
/// Benchmarks list themselves the same way with `: benchmark`, and are left
/// out: `--exact` on one runs it as a test, which measures nothing and takes as
/// long as a benchmark.
fn listed(out: &str) -> Vec<String> {
    out.lines()
        .filter_map(|l| l.strip_suffix(": test"))
        .map(|n| n.trim().to_string())
        .filter(|n| !n.is_empty())
        .collect()
}

/// Run one test in a process of its own and say what happened to it.
///
/// The output goes to a file rather than a pipe because the process may be
/// waited on with a timeout: a pipe nobody is draining fills and stops the
/// writer, so a test that printed enough would hang here and be reported as
/// the hang this is looking for. One file, reused, in the agent's own scratch
/// directory.
///
/// `timeout` is `None` for a caller that only wants the child's answer, and
/// then no clock is consulted at all — the wait blocks until the child is
/// done, however long the machine takes to get round to it. A budget is for
/// the one caller that cannot afford a hang, and it is a backstop rather than
/// an expectation: see `ALONE_TIMEOUT`.
/// Run one test in a process of its own and say what happened to it: one job of class
/// `check` in the tree's place, its budget the job's timeout. The manager ends
/// a job past it and says `timed_out`, which reads as the direct path's "no
/// answer … — killed"; the failure is parsed out of the job's log.
fn run_alone(jobs: &Jobs, tree: &Path, exe: &Path, name: &str, timeout: Option<Duration>) -> Option<Failure> {
    let argv = vec![relative(tree, exe).display().to_string(), "--exact".into(), name.to_string()];
    let limit = timeout.unwrap_or(BUILD_TIMEOUT);
    let (ended, text) = match jobs.run("alone", wsp_wire::host::JobClass::Check, tree, &[], argv, limit, &mut |_| {}) {
        Ok(r) => r,
        Err(e) => return Some(Failure { name: name.into(), at: None, message: Some(e) }),
    };
    if ended.timed_out || ended.hung {
        return Some(Failure { name: name.into(), at: None, message: Some(format!("no answer in {limit:?} — killed")) });
    }
    if ended.exit == Some(0) {
        return None;
    }
    Some(
        failures(&text)
            .into_iter()
            .find(|f| f.name == name)
            .unwrap_or(Failure { name: name.into(), at: None, message: None }),
    )
}

/// Every test in the suite, one process each, and the names of the ones that
/// did not survive it.
///
/// The tree and target are the agent's own — see the header for why this one
/// does not take a warm slot — and the compile has already happened by the time
/// anything here runs, so the 85s is test time and nothing else.
///
/// The printed failure is the record. There is no per-test log kept: the name,
/// the panic site and the assertion are what the suite's own red runs give and
/// what both investigations here worked from, and a named test is one command
/// to run again.
fn alone_pass(runs: &Jobs, p: &util::Paint, tree: &Path, exes: &[PathBuf], json_out: bool) -> (usize, Vec<Failure>) {
    let names: Vec<(PathBuf, String)> = exes
        .iter()
        .flat_map(|e| test_names(runs, tree, e).into_iter().map(move |n| (e.clone(), n)))
        .collect();
    if !json_out {
        println!(
            "{} {}",
            p.dim("alone"),
            p.bold(&format!("{} tests, one process each", names.len()))
        );
    }
    // Progress only to a terminal, and on one line. An agent's captured output
    // is charged for every byte of it on every later request, so seven hundred
    // counter lines in a transcript cost more than the ~90s did.
    let tick = !json_out && util::stdout_is_tty();
    let mut bad = Vec::new();
    for (i, (exe, name)) in names.iter().enumerate() {
        if tick {
            use std::io::Write;
            print!("\r{} {}/{}  ", p.dim("alone"), i + 1, names.len());
            let _ = std::io::stdout().flush();
        }
        if let Some(f) = run_alone(runs, tree, exe, name, Some(ALONE_TIMEOUT)) {
            if tick {
                print!("\r\x1b[K");
            }
            if !json_out {
                // Printed as it happens rather than collected: ~90s is long
                // enough that a failure at ten seconds is worth having at ten
                // seconds, and it is the only output this makes.
                match &f.at {
                    Some(at) => println!("{} {} {}", p.red("✗"), p.bold(&f.name), p.dim(at)),
                    None => println!("{} {}", p.red("✗"), p.bold(&f.name)),
                }
                if let Some(m) = &f.message {
                    println!("    {}", util::truncate(m, 200));
                }
            }
            bad.push(f);
        }
    }
    if tick {
        print!("\r\x1b[K");
        use std::io::Write;
        let _ = std::io::stdout().flush();
    }
    (names.len(), bad)
}

/// `wsp verify --alone`: compile the change, then run every test in the suite
/// in a process of its own, and print the ones that did not survive it.
///
/// Everything before this point is ordinary `verify` — the private index, the
/// patch, a tree reset to HEAD with the patch on it — so what is measured is
/// this agent's change and not whatever is in the checkout.
#[allow(clippy::too_many_arguments)]
fn alone(
    runs: &Jobs,
    p: &util::Paint,
    state: &Path,
    tree: &Path,
    target: &Path,
    json_out: bool,
    started: Instant,
) -> i32 {
    // The share covers the compile and is dropped before the pass. Ten minutes
    // of running one test at a time is not a build: a machine that counted it
    // as one would hand every other agent a smaller slice of the cores for the
    // whole of it, and this pass wants one core and the machine quiet.
    let exes = {
        let table = |h: &str| runs.table(h);
        let share = sharing::take(state, runs.live(&table));
        if !json_out {
            if let Some(note) = share.note() {
                println!("{} {}", p.dim("machine"), p.dim(&note));
            }
            println!("{} cargo test --workspace --no-run", p.dim("→"));
        }
        match test_binaries(runs, tree, target, &share) {
            Ok(e) => e,
            Err(msg) => {
                eprintln!("{msg}");
                eprintln!("{} nothing to run alone — the build failed", p.red("✗"));
                return 1;
            }
        }
    };

    let (n, bad) = alone_pass(runs, p, tree, &exes, json_out);
    let secs = started.elapsed().as_secs_f64();

    if json_out {
        println!(
            "{}",
            json!({
                "ok": bad.is_empty(),
                "alone": true,
                "tests": n,
                "tree": util::contract(tree),
                "failures": bad
                    .iter()
                    .map(|f| json!({"test": f.name, "at": f.at, "message": f.message}))
                    .collect::<Vec<_>>(),
                "seconds": (secs * 10.0).round() / 10.0,
            })
        );
        return i32::from(!bad.is_empty());
    }

    let took = util::duration_human(secs as i64);
    if bad.is_empty() {
        println!("{} {} in {took}", p.green("✓"), p.bold(&format!("{n} tests each passed alone")));
        0
    } else {
        // The count and nothing else: the names are already above, printed as
        // each one happened, and repeating them here would double the only
        // output this command has.
        println!(
            "{} {} in {took}",
            p.red("✗"),
            p.bold(&format!("{} of {n} failed alone", bad.len()))
        );
        1
    }
}

pub fn verify(store: &Store, args: &Args) -> i32 {
    let p = util::Paint::new();
    let json_out = args.json();

    let Ok(cwd) = std::env::current_dir() else {
        eprintln!("wsp: cannot read the current directory");
        return 2;
    };
    let Some(repo) = toplevel(&cwd) else {
        eprintln!("wsp: {} is not in a git repository", util::contract(&cwd));
        return 2;
    };

    let key = agent_key();
    // `repo` goes on being the tree under your hands, which is where the patch
    // comes from and where the worktree is added; `scratch` decides where the
    // build goes from what kind of tree that is.
    let Scratch { dir, tree, target, checkout } = scratch(store, &repo, &key);
    // `WSP_JOBS` decides how the runs below are run (the run's [`Jobs`]), once, before
    // anything is claimed: a warm lock says which kind of holder it is.
    let run = this_run(checkout.as_deref(), &key);
    let runs = match jobs_host(&store.state, &run) {
        Ok(host) => Jobs::new(host, run),
        Err(e) => {
            eprintln!("wsp: verify's jobs: {e}");
            return 2;
        }
    };
    let table = |h: &str| runs.table(h);
    let live = runs.live(&table);
    // The warm pool is keyed on the repository, and inside a checkout `repo` is
    // named for the task rather than the repository — `.worktrees/robustness-070`
    // — so the trunk is what names it. Otherwise every task would have a pool of
    // its own, which is the cold build this is trying to stop having.
    let named_for = crate::cmd_checkout::trunk(&repo).unwrap_or_else(|| repo.clone());

    // `--rm` before anything else: the point of it is a tree you can drop when
    // it has gone wrong, and needing a working repository to drop it would be
    // exactly backwards.
    if args.has("rm") {
        let all = args.has("all");
        // `worklist-051`. Everything above this line is a read, and everything
        // below is the same read acted on — which is what lets the two branches
        // share their words instead of describing each other.
        let dry = args.has("dry-run");
        let existed = tree.exists();
        // The warm tree first, because which one it was is written down *in*
        // the directory the next line removes — and, in the preview, because
        // holding the claim is what makes the answer true while it is printed.
        let warm_paths: Vec<String> = match dry {
            true => free_warm(store, live, &named_for, &dir, all).iter().map(|w| util::contract(&w.dir)).collect(),
            false => Vec::new(),
        };
        let warm = match dry {
            true => warm_paths.len(),
            false => clear_warm(store, live, &repo, &named_for, &dir, all),
        };
        if !dry {
            let _ = git(&repo, &["worktree", "remove", "--force", &tree.display().to_string()]);
            let _ = std::fs::remove_dir_all(&dir);
            let _ = git(&repo, &["worktree", "prune"]);
        }
        // `--all` is for the residue rather than for this run: the trees the
        // per-workspace keying left behind, 9.6G on this machine and reachable
        // by no other command, since the workspaces that owned them are gone.
        // Only under the state directory, and never a checkout's — a checkout's
        // belongs to a tree somebody may be standing in, and goes when that
        // does.
        //
        // `live` is kept rather than folded into the call because `None` and
        // `Some([])` produce the same empty residue and are not the same fact:
        // one is "compound did not answer, so this half was skipped" and the other
        // is "there is nothing here". Said out loud in both branches — a
        // preview more candid than the act would be the wrong way round.
        // The live keys are compound's seat list now: a build tree is keyed on
        // the agent that built in it, and a seat is that agent while it is held.
        let live: Option<Vec<String>> = if all { live_keys() } else { None };
        let residue: Vec<String> = if !all {
            Vec::new()
        } else if dry {
            stale_build_dirs(store, live.as_deref(), &dir).iter().map(|d| util::contract(d)).collect()
        } else {
            clear_build_dirs(store, live.as_deref(), &dir)
        };
        let backend_silent = all && live.is_none();
        if json_out {
            println!(
                "{}",
                json!({
                    "removed": existed,
                    "path": util::contract(&dir),
                    "also": residue.len(),
                    "also_paths": residue,
                    "warm": warm,
                    "warm_paths": warm_paths,
                    "backend_silent": backend_silent,
                    "dry_run": dry,
                })
            );
        } else {
            // `checkout --rm -n`'s words, deliberately: a reader who has learned
            // what "would remove" means on one removing verb has learned it on
            // all of them.
            let did = if dry { "would remove" } else { "removed" };
            if existed {
                println!("{did} {}", util::contract(&dir));
            } else {
                // Named by what owns it, which is the checkout inside one and
                // the agent outside — otherwise "no build tree for w20" reads
                // as a claim about the agent in the one arrangement where the
                // agent is not what the tree is keyed on.
                match &checkout {
                    Some(c) => println!("no build tree in {}", util::contract(c)),
                    None => println!("no build tree for {key}"),
                }
            }
            if !residue.is_empty() {
                println!("{did} {} left by earlier seats", residue.len());
            }
            if warm > 0 {
                let cold = if dry { "would be cold" } else { "is cold" };
                println!("{did} {warm} warm build tree(s) — the next build here {cold}");
            }
            // The paths, and only in the preview. The act prints counts because
            // by then the decision has been made and thirty lines is a wall;
            // the preview *is* the decision, and a count is not something
            // anybody can check. These are also the only place the residue is
            // ever named — nothing else in wsp lists it, which is most of why
            // this verb has a dry run at all.
            if dry {
                for path in warm_paths.iter().chain(residue.iter()) {
                    println!("  {}", p.dim(path));
                }
            }
            if backend_silent {
                println!(
                    "{}",
                    p.yellow(&format!(
                        "compound did not answer, so nothing left by earlier seats {}",
                        if dry { "is listed" } else { "was removed" }
                    ))
                );
            }
        }
        return 0;
    }

    if !repo.join("Cargo.toml").is_file() {
        eprintln!(
            "wsp: {} is not a cargo project — verify only knows how to build these",
            util::contract(&repo)
        );
        return 2;
    }

    let Some(head) = git(&repo, &["rev-parse", "HEAD"]).map(|s| s.trim().to_string()) else {
        eprintln!("wsp: {} has no HEAD to build against", util::contract(&repo));
        return 2;
    };

    if let Err(e) = std::fs::create_dir_all(&dir) {
        eprintln!("wsp: cannot make {}: {e}", util::contract(&dir));
        return 2;
    }

    // Step 1 and 2 of the commit procedure: a private index at HEAD, and the
    // file list read before the hunks.
    let index = dir.join("index");
    let patch_path = dir.join("patch.diff");
    let patch = match build_patch(&repo, &cwd, &index, &dir, &args.rest) {
        Ok(p) => p,
        Err(e) => {
            eprintln!("wsp: {e}");
            return 1;
        }
    };
    let files = staged_files(&index, &repo);
    if let Err(e) = std::fs::write(&patch_path, &patch) {
        eprintln!("wsp: cannot write {}: {e}", util::contract(&patch_path));
        return 2;
    }

    let left_out: Vec<String> =
        changed_files(&repo).into_iter().filter(|f| !files.contains(f)).collect();

    if !json_out {
        println!("{} {}", p.dim("head"), &head[..head.len().min(7)]);
        if files.is_empty() {
            println!("{}", p.dim("no change against HEAD — verifying HEAD itself"));
        } else {
            println!("{} {}", p.dim("under test"), p.bold(&format!("{} file(s)", files.len())));
            for f in &files {
                println!("  {f}");
            }
        }
        if !left_out.is_empty() {
            println!(
                "{} {}",
                p.yellow("not under test"),
                p.dim(&format!("{} file(s) changed and left out", left_out.len()))
            );
            for f in &left_out {
                println!("  {}", p.dim(f));
            }
        }
    }

    // A warm tree if there is one free, and the private cold one if there is
    // not — see [`crate::sharing`] for the 23s against 3s that makes this the
    // whole point of the command, and for why nobody queues for it. Held until
    // the build is done, and only the tree and its target move: the index, the
    // patch and the log stay in this agent's own directory, where two agents
    // cannot overwrite each other's.
    //
    // `--alone` is the exception, and the header says why: a warm slot is one
    // of three and that pass holds a tree for a minute and a half, against the
    // twenty seconds an ordinary build borrows one for. It builds in the
    // agent's own tree instead and pays 21s for it.
    let alone_pass_asked = args.has("alone");
    let warm = (!alone_pass_asked)
        .then(|| sharing::warm(&store.state, &named_for, sharing::WARM_TREES, live))
        .flatten();
    let (tree, target) = match &warm {
        Some(w) => (w.tree(), w.target()),
        None => (tree, target),
    };
    // Where this build put its artefacts, for `wsp install` to read: it looks
    // for the release binary this command produced, and after this change that
    // is no longer a path it can work out from the checkout alone. Not written
    // by `--alone`, which builds no release binary and would only point
    // `install` at a tree that has none.
    if !alone_pass_asked {
        let _ = std::fs::write(&dir.join(BUILT_AT), format!("{}\n", target.display()));
    }

    let started = Instant::now();
    let fresh = match ensure_tree(&repo, &tree, &head) {
        Ok(f) => f,
        Err(e) => {
            eprintln!("wsp: {e}");
            return 1;
        }
    };
    if !json_out && fresh {
        println!("{} {}", p.dim("new build tree"), util::contract(&tree));
    }

    if !patch.trim().is_empty() {
        if let Err(e) = git_ok(&tree, &["apply", "--binary", &patch_path.display().to_string()]) {
            eprintln!("wsp: the change does not apply to HEAD: {e}");
            eprintln!("wsp: the patch is at {}", util::contract(&patch_path));
            return 1;
        }
    }

    if alone_pass_asked {
        return alone(&runs, &p, &store.state, &tree, &target, json_out, started);
    }

    let mut steps: Vec<(&str, Vec<&str>)> = Vec::new();
    if args.has("check") {
        steps.push(("check", vec!["check", "--workspace", "--quiet"]));
    } else {
        steps.push(("test", vec!["test", "--workspace", "--quiet"]));
    }
    if args.has("release") {
        steps.push(("release", vec!["build", "--release", "--quiet"]));
    }

    // Registered here rather than at the top of the command: the share is
    // held for as long as it is being used, so a verify that spends a minute
    // resolving a patch is not counted against the builds running while it does.
    let share = sharing::take(&store.state, live);
    if !json_out {
        if let Some(note) = share.note() {
            println!("{} {}", p.dim("machine"), p.dim(&note));
        }
    }

    let mut failed: Option<&str> = None;
    let mut output = String::new();
    for (name, argv) in &steps {
        if !json_out {
            println!("{} cargo {}", p.dim("→"), argv.join(" "));
        }
        // Under `manager` the warm tree's lock is touched before each job, so
        // its holder reads as there from the table; a lock that is no longer
        // this run's means the tree was taken, and nothing runs in it.
        if let Some(w) = &warm {
            if !w.touch() {
                output.push_str("wsp: the warm tree was taken by another build before this one ran in it\n");
                failed = Some(name);
                break;
            }
        }
        let (ok, text) = cargo_job(&runs, &tree, &target, argv, !json_out, &share);
        output.push_str(&text);
        if !ok {
            failed = Some(name);
            break;
        }
    }
    let secs = started.elapsed().as_secs_f64();

    // Everything a red run knows, taken before anything else can happen to it.
    // The log goes beside `patch.diff` in the scratch directory — which the
    // tree it belongs to already owns and already removes — so keeping it costs
    // nothing that was not already being kept, and a path in the error line is
    // worth more than any summary printed in its place.
    let mut log: Option<PathBuf> = None;
    let mut failed_tests: Vec<Failure> = Vec::new();
    let mut alone: Option<bool> = None;
    if failed.is_some() {
        let path = dir.join("cargo.log");
        if std::fs::write(&path, &output).is_ok() {
            log = Some(path);
        }
        failed_tests = failures(&output);
        if let Some(first) = failed_tests.first() {
            let held = warm.as_ref().is_none_or(|w| w.touch());
            if held {
                alone = rerun_alone(&runs, &tree, &target, &first.name, &share);
            }
        }
    }

    // A row copied to an executor records the tree hash this ran on, so `review`
    // can refuse a tree that is not the one that passed (`docs/remote-agents.md`
    // §6). Nothing happens for a row that was never copied.
    if failed.is_none() {
        if let Some(row) = repo.file_name().map(|n| n.to_string_lossy().into_owned()) {
            crate::treecopy::note_verified(store, &row, &repo);
        }
    }

    if json_out {
        // The tail rather than the whole thing: a failing cargo run is
        // thousands of lines and the last eighty hold the error.
        let tail: Vec<&str> = output.lines().rev().take(80).collect::<Vec<_>>().into_iter().rev().collect();
        println!(
            "{}",
            json!({
                "ok": failed.is_none(),
                "failed": failed,
                "head": head,
                "agent": key,
                "files": files,
                "left_out": left_out,
                "tree": util::contract(&tree),
                "checkout": checkout.as_deref().map(util::contract),
                "patch": util::contract(&patch_path),
                "log": log.as_deref().map(util::contract),
                "failures": failed_tests
                    .iter()
                    .map(|f| json!({"test": f.name, "at": f.at, "message": f.message}))
                    .collect::<Vec<_>>(),
                "alone": alone,
                "jobs": share.jobs,
                "cores": share.cores,
                "builds": share.live,
                "warm": warm.as_ref().map(|w| w.slot),
                "seconds": (secs * 10.0).round() / 10.0,
                "output": tail.join("\n"),
            })
        );
        return i32::from(failed.is_some());
    }

    match failed {
        None => {
            println!("{} {} in {:.0}s", p.green("✓"), p.bold("verified against HEAD"), secs);
            println!("{}", p.dim(&format!("tree kept warm at {}", util::contract(&tree))));
            0
        }
        Some(step) => {
            println!("{} cargo {step} failed in {:.0}s", p.red("✗"), secs);
            for f in &failed_tests {
                match &f.at {
                    Some(at) => println!("  {} {}", p.bold(&f.name), p.dim(at)),
                    None => println!("  {}", p.bold(&f.name)),
                }
                if let Some(m) = &f.message {
                    println!("    {}", util::truncate(m, 200));
                }
            }
            if let Some(first) = failed_tests.first() {
                match alone {
                    // The flake signature, and the reason the re-run happens at
                    // all. Yellow because it is the interesting answer, not
                    // green: the suite is still red and still failed.
                    Some(true) => {
                        println!(
                            "{}",
                            p.yellow(&format!("{} failed in the suite, passed alone", first.name))
                        );
                        // The one moment this pass is mechanised into, and the
                        // reason it costs a line here rather than a paragraph
                        // in a document: twice now this exact signature has
                        // meant a test that is broken outright, with the suite's
                        // own concurrency hiding it. See the header.
                        println!(
                            "{}",
                            p.dim("wsp verify --alone runs every test in its own process (~90s) — twice now this signature has meant broken, not flaky")
                        );
                    }
                    Some(false) => println!("{}", p.dim(&format!("{} fails alone too", first.name))),
                    None => {}
                }
            }
            // "left at" is now only true until somebody else claims that tree
            // and resets it to their HEAD, which is the price of it being warm.
            // What survives is the log and the patch: they are written in this
            // agent's own directory precisely so that a red run's evidence does
            // not depend on a tree it does not own.
            match &log {
                Some(l) => println!(
                    "{}",
                    p.dim(&format!(
                        "built in {} — cargo output {}, patch {}",
                        util::contract(&tree),
                        util::contract(l),
                        util::contract(&patch_path)
                    ))
                ),
                None => println!(
                    "{}",
                    p.dim(&format!(
                        "built in {} — the patch is {}",
                        util::contract(&tree),
                        util::contract(&patch_path)
                    ))
                ),
            }
            1
        }
    }
}

/// The verbs this module answers to, each with the help it carries. `main` and
/// the help text are assembled from these; nothing in `main.rs` names them.
pub const VERBS: &[crate::verbs::Verb] = &[
    crate::verbs::Verb {
        names: &["verify"],
        run: verify,
        help: &[
            crate::verbs::Help {
                section: crate::verbs::Section::Agents,
                order: 150,
                text: r##"  wsp verify [<path>…] [--check] [--release] [--rm [--all] [-n]]
                                    build and test your change at HEAD, in one
                                    of a few warm trees this machine shares —
                                    yours alone while it builds, and cold only
                                    when they are all busy; --rm drops the one
                                    you built in, --all every free one and the
                                    build residue no other verb can reach. -n
                                    names all of it and removes none"##,
            },
            crate::verbs::Help {
                section: crate::verbs::Section::Agents,
                order: 160,
                text: r##"  wsp verify --alone                …or every test in a process of its own,
                                    ~90s, naming the failures and nothing
                                    else — what to reach for when a test goes
                                    red and then green"##,
            },
        ],
    },
];

#[cfg(test)]
mod tests {
    use super::*;

    /// A test that never answers has to be a *result*, not a hung pass.
    ///
    /// The alone-pass is meant to be started and left — that is the whole
    /// difference between an instrument and somebody watching a terminal — and
    /// a deadlock is precisely the class of bug it hunts (`robustness-072` was
    /// a wait on a file that never appeared). Without the timeout the first
    /// test to hang costs the whole pass and prints nothing at the end of it,
    /// which is the one way this can fail silently.
    ///
    /// Nothing here is timed. The failure *is* the proof the budget fired:
    /// waiting the child out would have produced a pass, because the child
    /// exits zero. And the child sleeps five minutes rather than a few seconds
    /// so that the pass and the failure are not separated by a number a
    /// descheduled parent could cross — `exec` so the kill lands on the sleep
    /// itself and no orphan is left behind by the one that never happens.
    #[test]
    fn a_test_that_never_answers_is_a_failure_rather_than_a_hung_pass() {
        let dir = scratch_dir("alone-hang");
        let exe = dir.join("hangs");
        wsp_testkit::script(&exe, "#!/bin/sh\nexec sleep 300\n");

        let out = run_alone(&inproc_runs(&dir), &dir.join("tree"), &exe, "whatever::hangs", Some(Duration::from_millis(150)));

        let f = out.expect("a test that never returned was reported as passing");
        assert_eq!(f.name, "whatever::hangs");
        assert!(
            f.message.as_deref().unwrap_or_default().contains("no answer"),
            "the failure does not say it was killed: {:?}",
            f.message
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// A budget cannot tell a slow test from a hung one, so only the caller
    /// that has to survive a hang carries one.
    ///
    /// This is the whole argument for `timeout` being an `Option`. A wall-clock
    /// budget answers "has this taken too long", and on a machine with six
    /// agents building, "too long" is a fact about the machine. Held against a
    /// test that was going to answer, it prints the accusation this instrument
    /// exists to make — *this test does not survive alone* — about a test that
    /// does. The child below is the same shape as the `exit 0` script that was
    /// reported as never answering, only slowed to three seconds (a job's bound is whole seconds) so it
    /// can say so without a loaded machine.
    #[test]
    fn a_slow_test_is_not_a_hung_one_unless_a_budget_was_asked_for() {
        let dir = scratch_dir("alone-slow");
        let exe = dir.join("slow");
        wsp_testkit::script(&exe, "#!/bin/sh\nsleep 3\nexit 0\n");
        let runs = inproc_runs(&dir);
        let tree = dir.join("tree");

        // A budget it cannot meet accuses a test that passes.
        let accused = run_alone(&runs, &tree, &exe, "a::b", Some(Duration::from_secs(1)))
            .expect("the budget did not fire on a child that outlives it");
        assert!(
            accused.message.as_deref().unwrap_or_default().contains("no answer"),
            "a child killed by the budget was reported as something else: {:?}",
            accused.message
        );

        // The same child, no budget, and there is no clock left to be wrong.
        assert!(
            run_alone(&runs, &tree, &exe, "a::b", None).is_none(),
            "a test that passed was reported as a failure"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// A red test alone reads exactly as it does in the suite, because it is
    /// the same parser: the pass adds a loop, not a second opinion about what a
    /// failure is.
    #[test]
    fn a_test_that_fails_alone_keeps_the_assertion_the_suite_would_have_shown() {
        let dir = scratch_dir("alone-red");
        let exe = dir.join("fails");
        // libtest's own shape, which is what the real binary prints.
        wsp_testkit::script(
            &exe,
            "#!/bin/sh\ncat <<'OUT'\nrunning 1 test\ntest a::b ... FAILED\n\nfailures:\n\n---- a::b stdout ----\nthread 'a::b' panicked at src/a.rs:12:9:\nthe seat was empty\n\nfailures:\n    a::b\n\ntest result: FAILED. 0 passed; 1 failed\nOUT\nexit 101\n",
        );

        // No budget: this is about what the parser makes of a red test, and a
        // clock here has nothing to measure and one thing to get wrong. With
        // five seconds it did — 11 of 150 runs on a loaded machine reported a
        // script that is `exit 0` as a test that never answered, which is the
        // instrument accusing the suite of the fault it was built to find.
        let runs = inproc_runs(&dir);
        let f = run_alone(&runs, &dir.join("tree"), &exe, "a::b", None)
            .expect("a failing test was reported as passing");
        assert_eq!(f.at.as_deref(), Some("src/a.rs:12:9"), "the panic site was lost");
        assert_eq!(f.message.as_deref(), Some("the seat was empty"), "the assertion was lost");

        let ok = dir.join("passes");
        wsp_testkit::script(&ok, "#!/bin/sh\nexit 0\n");
        assert!(
            run_alone(&runs, &dir.join("tree"), &ok, "a::b", None).is_none(),
            "a passing test was reported as a failure"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// What gets run, and what it gets run against, are both read out of
    /// something else's output — so both are parsed rather than assumed.
    ///
    /// The `lib` artifact in the middle is the one that matters: it is a real
    /// line from a real `cargo test --no-run`, it has no executable, and taking
    /// it would put a `None` in the list of things to run.
    #[test]
    fn the_binaries_come_from_cargos_json_and_the_names_from_the_binary() {
        let text = concat!(
            r#"{"reason":"compiler-artifact","target":{"name":"wsp","kind":["lib"]},"#,
            r#""profile":{"test":false},"executable":null}"#,
            "
",
            r#"{"reason":"compiler-artifact","target":{"name":"wsp","kind":["bin"]},"#,
            r#""profile":{"test":true},"executable":"/t/deps/wsp-1a2b"}"#,
            "
",
            "   Compiling wsp v0.1.0
",
            r#"{"reason":"build-finished","success":true}"#,
        );
        assert_eq!(executables(text), vec![PathBuf::from("/t/deps/wsp-1a2b")]);

        assert_eq!(
            listed("store::tests::a: test
bench::b: benchmark

2 tests, 1 benchmark
"),
            vec!["store::tests::a".to_string()],
            "the count line or the benchmark was taken for a test"
        );
    }

    /// A verify's jobs on `wsp-host`'s runner in this process, the tree root
    /// the parent of `dir`: a tree is `dir/tree` and a script beside it is
    /// `../script`.
    fn inproc_runs(dir: &Path) -> Jobs<'static> {
        let root = dir.parent().unwrap().to_path_buf();
        crate::job_place::set_tree_root(Some(root.clone()));
        let host: &'static crate::host::InProcess =
            Box::leak(Box::new(crate::host::InProcess::new().with_jobs(&dir.join("jobs-exec"), &root)));
        let run = sharing::Run { host: util::hostname(), row: "t".into(), attempt: 0, mark: "m".into() };
        Jobs::new(host, run)
    }

    /// Whether `pid` runs, for the warm-tree tests' locks.
    fn alive(pids: &[u32]) -> std::collections::BTreeSet<u32> {
        extern "C" {
            fn kill(pid: i32, sig: i32) -> i32;
        }
        // SAFETY: signal 0 only asks.
        pids.iter().copied().filter(|p| unsafe { kill(*p as i32, 0) } == 0).collect()
    }

    fn scratch_dir(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("wsp-verify-{}-{}", name, std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    /// A repository with one commit, and `git` configured enough to make it.
    ///
    /// The commit is made against a git of its own: an empty global config
    /// under the test's directory, no system config, and no signing — so a
    /// machine whose global config signs every commit (`commit.gpgsign=true`
    /// with no key) still gets its fixture. All of it is set on the child, so
    /// no other test's environment is touched.
    fn repo(dir: &Path) {
        let home = dir.join(".test-home");
        std::fs::create_dir_all(&home).unwrap();
        let global = home.join("gitconfig");
        std::fs::write(&global, "").unwrap();
        let run = |args: &[&str]| {
            let out = Command::new("git")
                .arg("-C")
                .arg(dir)
                .args(["-c", "commit.gpgsign=false"])
                .args(args)
                .env_remove("GIT_INDEX_FILE")
                .env("HOME", &home)
                .env("GIT_CONFIG_GLOBAL", &global)
                .env("GIT_CONFIG_NOSYSTEM", "1")
                .env("GIT_AUTHOR_NAME", "t")
                .env("GIT_AUTHOR_EMAIL", "t@t")
                .env("GIT_COMMITTER_NAME", "t")
                .env("GIT_COMMITTER_EMAIL", "t@t")
                .output()
                .unwrap();
            assert!(out.status.success(), "git {args:?}: {}", String::from_utf8_lossy(&out.stderr));
        };
        run(&["init", "--quiet", "-b", "master"]);
        // The fixture's own git home is no part of the tree under test.
        std::fs::write(dir.join(".git/info/exclude"), ".test-home/\n").unwrap();
        std::fs::write(dir.join("kept.txt"), "one\n").unwrap();
        run(&["add", "kept.txt"]);
        run(&["commit", "--quiet", "-m", "first"]);
    }

    /// The failure this command exists to make impossible, asserted directly.
    ///
    /// `git worktree add` writes the new worktree's index, and with
    /// `GIT_INDEX_FILE` exported it writes it over the private one — so the
    /// patch comes back empty and `git apply` refuses it, which reads like a
    /// staging mistake rather than what it is. Every git call here strips the
    /// variable, so a caller partway through `wsp commit-help` with one
    /// exported still gets its own staging read correctly.
    #[test]
    fn a_caller_holding_a_private_index_still_gets_its_own_diff() {
        // `GIT_INDEX_FILE` is process-wide, so it is exported in a process of
        // its own — this test binary again, running only
        // `patch_built_under_an_exported_index` — and never over the threads
        // of the other tests here, which would read this staging as theirs.
        let dir = scratch_dir("index");
        repo(&dir);
        std::fs::write(dir.join("kept.txt"), "one\ntwo\n").unwrap();

        // Somebody else's index, exported over us, holding nothing at all.
        let theirs = dir.join("their-index");
        let out = Command::new(std::env::current_exe().unwrap())
            .args(["--exact", "cmd_verify::tests::patch_built_under_an_exported_index"])
            .args(["--ignored", "--nocapture", "--test-threads=1"])
            .env("GIT_INDEX_FILE", &theirs)
            .env(EXPORTED_INDEX_REPO, &dir)
            .output()
            .unwrap();
        let said = format!("{}{}", String::from_utf8_lossy(&out.stdout), String::from_utf8_lossy(&out.stderr));
        assert!(out.status.success(), "the child failed:\n{said}");
        let patch = std::fs::read_to_string(dir.join("scratch/patch-out"))
            .unwrap_or_else(|e| panic!("the child built no patch ({e}):\n{said}"));

        assert!(patch.contains("+two"), "the change was not in the patch:\n{patch}");
        assert!(!theirs.exists(), "we wrote to the caller's index");
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// Names the repository `patch_built_under_an_exported_index` works in.
    const EXPORTED_INDEX_REPO: &str = "WSP_TEST_EXPORTED_INDEX_REPO";

    /// The half of `a_caller_holding_a_private_index_still_gets_its_own_diff`
    /// that runs with `GIT_INDEX_FILE` exported: only ever in the child that
    /// test starts, which sets both variables. Run any other way it does
    /// nothing.
    #[test]
    #[ignore = "run by a_caller_holding_a_private_index_still_gets_its_own_diff, in a process of its own"]
    fn patch_built_under_an_exported_index() {
        let Some(dir) = std::env::var_os(EXPORTED_INDEX_REPO).map(PathBuf::from) else {
            return;
        };
        assert!(std::env::var_os("GIT_INDEX_FILE").is_some(), "the caller's index was not exported");
        let scratch = dir.join("scratch");
        std::fs::create_dir_all(&scratch).unwrap();
        let mine = scratch.join("index");
        let patch = build_patch(&dir, &dir, &mine, &scratch, &[]).unwrap();
        std::fs::write(scratch.join("patch-out"), patch).unwrap();
    }

    /// A new file is the commonest thing an agent adds and the easiest thing
    /// for a diff to miss — `git diff HEAD` does not see it, and a build that
    /// silently leaves it out goes green for a change that does not compile.
    #[test]
    fn a_file_git_has_never_seen_is_still_under_test() {
        let dir = scratch_dir("untracked");
        repo(&dir);
        std::fs::write(dir.join("new.rs"), "fn f() {}\n").unwrap();

        // Deliberately inside the repository, which is the case the exclude
        // guard is for: a state directory under the tree would otherwise put
        // verify's own index and patch into the patch.
        let scratch = dir.join("scratch");
        std::fs::create_dir_all(&scratch).unwrap();
        let index = scratch.join("index");
        let patch = build_patch(&dir, &dir, &index, &scratch, &[]).unwrap();
        assert!(patch.contains("new.rs"), "an untracked file was not in the patch:\n{patch}");
        assert_eq!(
            staged_files(&index, &dir),
            vec!["new.rs".to_string()],
            "verify staged its own scratch directory"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// Naming paths is the whole discipline: the tree is shared, so "everything
    /// that changed" is not the same question as "what I changed".
    #[test]
    fn naming_paths_leaves_the_other_agents_work_out_of_it() {
        let dir = scratch_dir("paths");
        repo(&dir);
        std::fs::write(dir.join("mine.txt"), "mine\n").unwrap();
        std::fs::write(dir.join("theirs.txt"), "theirs\n").unwrap();

        let scratch = dir.join("scratch");
        std::fs::create_dir_all(&scratch).unwrap();
        let index = scratch.join("index");
        let patch = build_patch(&dir, &dir, &index, &scratch, &["mine.txt".to_string()]).unwrap();
        assert!(patch.contains("mine.txt"));
        assert!(!patch.contains("theirs.txt"), "another agent's file rode along:\n{patch}");
        assert_eq!(staged_files(&index, &dir), vec!["mine.txt".to_string()]);
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// Naming paths keeps somebody else's work out, and lets you leave out your
    /// own. The second is the dangerous one: a new module nothing declares
    /// compiles perfectly, because nothing compiles it — a green build for a
    /// change that does not build. So what was left out has to be visible
    /// beside what was taken.
    #[test]
    fn what_you_left_out_is_reported_next_to_what_you_named() {
        let dir = scratch_dir("leftout");
        repo(&dir);
        std::fs::write(dir.join("mine.txt"), "mine\n").unwrap();
        std::fs::write(dir.join("forgot.txt"), "also mine\n").unwrap();

        let scratch_dir = dir.join("scratch");
        std::fs::create_dir_all(&scratch_dir).unwrap();
        let index = scratch_dir.join("index");
        build_patch(&dir, &dir, &index, &scratch_dir, &["mine.txt".to_string()]).unwrap();

        let named = staged_files(&index, &dir);
        let left_out: Vec<String> =
            changed_files(&dir).into_iter().filter(|f| !named.contains(f)).collect();
        assert_eq!(named, vec!["mine.txt".to_string()]);
        assert!(
            left_out.contains(&"forgot.txt".to_string()),
            "a changed file left out of the build was not reported: {left_out:?}"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// The tree is keyed on the workspace rather than the pane because an agent
    /// is a workspace and not a pane: it splits, and every pane it opens is the
    /// same agent wanting the same warm tree.
    #[test]
    fn the_build_tree_is_this_agents_and_not_this_panes() {
        // `WSP_AGENT` and `WSP_BACKEND_WORKSPACE_ID` are what every caller of
        // `agent_key` reads, and this test removes one of them — so a test in
        // another thread asking which agent it is would be answered by this
        // one's teardown. Bare rather than `isolated`: `Store::at` is explicit
        // here and nothing reaches a backend.
        let _env = util::env_lock();
        std::env::set_var("WSP_AGENT", "w1");
        assert_eq!(agent_key(), "w1");
        std::env::set_var("WSP_AGENT", "  ");
        std::env::remove_var("WSP_BACKEND_WORKSPACE_ID");
        assert_eq!(agent_key(), "solo");
        std::env::remove_var("WSP_AGENT");

        let store = Store::at(PathBuf::from("/tmp/store"), PathBuf::from("/tmp/state"));
        let a = build_dir(&store, Path::new("/Users/x/claude/wsp"), "w1");
        let b = build_dir(&store, Path::new("/Users/x/claude/wsp"), "w2");
        assert_ne!(a, b, "two agents shared one build tree");
        assert!(a.starts_with("/tmp/state/build"), "the build tree escaped WSP_STATE: {a:?}");

        // The trunk names it, so an agent that moves task keeps the tree it
        // warmed. What resolves the trunk is tested where it lives, in
        // `cmd_checkout`.
        assert_eq!(
            a,
            build_dir(&store, Path::new("/Users/x/claude/wsp"), "w1"),
            "one agent, two tasks, two build trees"
        );
    }

    /// The saving this command was rebuilt for: an agent standing in its own
    /// checkout builds *inside* it, so the build goes when the tree does and
    /// what was unbounded becomes one per live task.
    #[test]
    fn inside_a_checkout_the_build_goes_in_the_checkout_and_dies_with_it() {
        let store = Store::at(PathBuf::from("/tmp/store"), PathBuf::from("/tmp/state"));
        let checkout = PathBuf::from("/Users/x/claude/wsp/.worktrees/robustness-046");

        let sc = scratch(&store, &checkout, "w1");
        assert_eq!(sc.checkout.as_deref(), Some(checkout.as_path()));
        for p in [&sc.dir, &sc.tree, &sc.target] {
            assert!(p.starts_with(&checkout), "{p:?} outlives the tree it belongs to");
            // Under `target/`, which every cargo project already gitignores. A
            // scratch directory git could see would make the checkout
            // permanently dirty, and `checkout --sweep` refuses to remove a
            // dirty tree — so the leak this closes would come straight back.
            assert!(p.starts_with(checkout.join("target")), "{p:?} is not gitignored");
        }
    }

    /// The measurement that settled the shape, asserted so it cannot be
    /// quietly undone. Pointing both trees at one `CARGO_TARGET_DIR` is the
    /// obvious saving and it is wrong: cargo records a unit's dependencies as
    /// absolute paths and judges freshness by their mtimes, so a build in the
    /// scratch tree leaves the *checkout's* fingerprint pointing at the scratch
    /// tree's sources. The agent's next `cargo test` then asks whether the
    /// scratch tree changed, prints `Fresh`, and reruns the old binary —
    /// observed here on 2026-08-17, a green 482 tests for source that had never
    /// been compiled. A wrong green is what this whole command exists to stop.
    #[test]
    fn the_scratch_never_builds_into_the_tree_the_agent_edits_in() {
        let store = Store::at(PathBuf::from("/tmp/store"), PathBuf::from("/tmp/state"));
        let checkout = PathBuf::from("/Users/x/claude/wsp/.worktrees/robustness-046");
        let sc = scratch(&store, &checkout, "w1");
        assert_ne!(
            sc.target,
            checkout.join("target"),
            "the scratch shares the checkout's target dir — `cargo test` there now lies"
        );
        assert_eq!(sc.target, sc.dir.join("target"), "the scratch built somewhere it does not own");
    }

    /// The trunk is still shared — the coordination seat stands there — so the
    /// original argument still holds there, and only there.
    #[test]
    fn in_the_trunk_an_agent_still_builds_in_a_tree_of_its_own() {
        let store = Store::at(PathBuf::from("/tmp/store"), PathBuf::from("/tmp/state"));
        let sc = scratch(&store, Path::new("/tmp/not-a-checkout"), "w1");
        assert_eq!(sc.checkout, None);
        assert!(
            sc.target.starts_with("/tmp/state/build"),
            "the trunk's build escaped WSP_STATE: {:?}",
            sc.target
        );
        assert_eq!(sc.target, sc.dir.join("target"), "the trunk shares its target dir");
    }

    /// A build no longer lands where the checkout says it does — it lands in
    /// whichever warm tree was free — so the one caller that wants the
    /// *artefacts* rather than a place to build has to be told, and this is the
    /// telling. `wsp install` looking in the wrong place is not a small bug:
    /// it falls through to `target/release` and installs somebody else's build
    /// while reporting it as yours.
    #[test]
    fn install_is_told_which_tree_the_build_actually_went_to() {
        let iso = util::isolated("verify-built-at");
        let store = Store::at(iso.home(), iso.state());
        let repo = iso.path("wsp");
        let sc = scratch(&store, &repo, "w1");
        std::fs::create_dir_all(&sc.dir).unwrap();
        let warm = iso.path("state/warm/wsp-0/target");
        std::fs::create_dir_all(&warm).unwrap();
        std::fs::write(sc.dir.join(BUILT_AT), format!("{}\n", warm.display())).unwrap();

        let found = last_build(&store, &repo, "w1");
        assert_eq!(found.target, warm, "install would have looked in the private tree");
        assert_eq!(found.tree, warm.parent().unwrap().join("tree"), "the tree beside it");
        assert_eq!(found.dir, sc.dir, "the patch and the log are still this agent's own");
    }

    /// And a pointer at a tree that has been removed is not an answer. `--rm`
    /// takes warm trees away, so the pointer outliving one is ordinary rather
    /// than exotic — and "no build" is the right answer then, not a path that
    /// no longer exists.
    #[test]
    fn a_pointer_at_a_tree_that_is_gone_falls_back_to_this_agents_own() {
        let iso = util::isolated("verify-built-gone");
        let store = Store::at(iso.home(), iso.state());
        let repo = iso.path("wsp");
        let sc = scratch(&store, &repo, "w1");
        std::fs::create_dir_all(&sc.dir).unwrap();
        std::fs::write(sc.dir.join(BUILT_AT), "/tmp/warm-tree-that-was-removed/target\n").unwrap();

        assert_eq!(last_build(&store, &repo, "w1").target, sc.target);
    }

    /// The residue of keying on the workspace, and the only command that can
    /// reach it: the workspaces that named those trees are gone, so no agent
    /// alive can name one. 9.6G across 30 of them, measured 2026-08-17.
    ///
    /// A live workspace keeps its tree. The name is `<repo>-<workspace>` and a
    /// repository name may hold a dash, so the match is on the tail — `wsp-w2x`
    /// and `my-wsp-w2x` are both workspace `w2x`, and neither is workspace `2x`.
    #[test]
    fn a_tree_is_cleared_when_its_workspace_is_gone_and_kept_while_it_is_not() {
        let dir = scratch_dir("residue");
        let store = Store::at(dir.join("store"), dir.join("state"));
        let build = store.state.join("build");
        for t in ["wsp-w2x", "wsp-w2y", "my-wsp-w2z"] {
            std::fs::create_dir_all(build.join(t).join("tree")).unwrap();
        }
        let live = vec!["w2y".to_string(), "w2z".to_string()];

        let gone = clear_build_dirs(&store, Some(&live), Path::new("/nowhere"));
        assert_eq!(gone.len(), 1, "the wrong trees went: {gone:?}");
        assert!(!build.join("wsp-w2x").exists(), "a tree nobody can name was left behind");
        assert!(build.join("wsp-w2y").exists(), "a live agent lost the tree it was building in");
        assert!(build.join("my-wsp-w2z").exists(), "the workspace was matched as part of the repo");

        // Nothing to clear is not an error: `--rm --all` is a thing you run
        // without first checking whether there is anything to run it on.
        assert!(clear_build_dirs(&store, Some(&live), Path::new("/nowhere")).is_empty());
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// The feed for the residue sweep: a seat compound holds keeps its tree,
    /// the shell's `solo` key is never residue, and a census nobody answered
    /// is no list at all — so the sweep sees a live seat and a residue, and
    /// removes nothing on silence.
    #[test]
    fn the_residue_sweep_is_fed_by_the_seats_compound_holds() {
        use crate::place::{Census, Refusal, Seat, Seated, State};
        let dir = scratch_dir("seat-feed");
        let store = Store::at(dir.join("store"), dir.join("state"));
        let build = store.state.join("build");
        for t in ["wsp-cpd-1", "wsp-cpd-2", "wsp-solo"] {
            std::fs::create_dir_all(build.join(t).join("tree")).unwrap();
        }
        let seated = Seated { seat: Seat::new("cpd-1"), state: State::Working, ..Default::default() };
        let live = keys_of(&Census::heard("", vec![seated])).expect("an answered census is a list");

        let said = stale_build_dirs(&store, Some(&live), Path::new("/nowhere"));
        assert_eq!(said, vec![build.join("wsp-cpd-2")], "the residue is the tree no seat holds");
        assert_eq!(clear_build_dirs(&store, Some(&live), Path::new("/nowhere")).len(), 1);
        assert!(build.join("wsp-cpd-1").exists(), "a live seat lost the tree it was building in");
        assert!(build.join("wsp-solo").exists(), "a shell's tree was taken for residue");

        assert!(keys_of(&Census::silent("", Refusal::Unreachable("down".into()))).is_none());
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// The narrow half, aimed at one workspace, and it must stay narrow.
    ///
    /// `wsp despawn` calls this the moment a seat's workspace goes, so the
    /// blast radius is the difference between the two functions: this one is
    /// *told* which workspace has died, and the sweep above infers it from a
    /// list of the living. An off-by-one there costs a running agent its build
    /// tree, which is why the suffix rule is asserted here too rather than
    /// trusted from one caller away.
    #[test]
    fn ending_one_workspace_clears_its_build_trees_and_nobody_elses() {
        let dir = scratch_dir("one-key");
        let store = Store::at(dir.join("store"), dir.join("state"));
        let build = store.state.join("build");
        for t in ["wsp-w2x", "backend-w2x", "wsp-w2y", "my-wsp-w2x"] {
            std::fs::create_dir_all(build.join(t).join("tree")).unwrap();
        }

        let gone = clear_build_key(&store, "w2x");
        assert_eq!(gone.len(), 3, "the wrong trees went: {gone:?}");
        assert!(!build.join("wsp-w2x").exists());
        assert!(!build.join("backend-w2x").exists(), "a workspace can build more than one repository");
        assert!(!build.join("my-wsp-w2x").exists(), "the workspace was matched as part of the repo name");
        assert!(build.join("wsp-w2y").exists(), "another workspace lost the tree it was building in");

        // A workspace with nothing keyed on it is the ordinary case — most
        // agents build in the warm pool — and it is not an error.
        assert!(clear_build_key(&store, "w2x").is_empty());
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// The one line standing between a backend that is down and every running
    /// agent losing the tree it is mid-build in. A backend that cannot be reached
    /// reports no workspaces, which looks exactly like a machine with nothing
    /// running on it — so `None` removes nothing, and an empty list is believed
    /// only because backend said it. The same judgement `may_reap` makes, and for
    /// the same reason.
    #[test]
    fn a_backend_that_did_not_answer_is_not_a_machine_with_no_agents_on_it() {
        let dir = scratch_dir("silence");
        let store = Store::at(dir.join("store"), dir.join("state"));
        let build = store.state.join("build");
        std::fs::create_dir_all(build.join("wsp-w2x").join("tree")).unwrap();

        assert!(
            clear_build_dirs(&store, None, Path::new("/nowhere")).is_empty(),
            "silence from backend was read as nothing running"
        );
        assert!(build.join("wsp-w2x").exists(), "a tree went on backend saying nothing");

        // Said, and meant: an empty answer from a backend that answered is a
        // machine with no agents on it, and its trees are residue.
        assert_eq!(clear_build_dirs(&store, Some(&[]), Path::new("/nowhere")).len(), 1);
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// `worklist-051`, the residue half. `-n` on `--rm --all` is only worth
    /// having if it is a promise, so the promise is what is asserted:
    /// [`stale_build_dirs`] is asked which trees would go, and then
    /// [`clear_build_dirs`] is asked to go and remove them, and the two lists
    /// have to be the same one.
    ///
    /// This is the strongest case for a dry run in the whole of `worklist-051`
    /// and the fixture says why in three directories. What `--all` matches is a
    /// listing of a directory nobody types crossed with backend's live workspace
    /// list — a wildcard over two things the caller cannot see from where they
    /// are standing. And the removal *does not name what it took*: it prints a
    /// count. So the preview is not a convenience here, it is the only place
    /// these paths are ever said out loud.
    #[test]
    fn what_a_dry_run_of_rm_all_names_is_what_the_removal_then_takes() {
        let dir = scratch_dir("foresee-residue");
        let store = Store::at(dir.join("store"), dir.join("state"));
        let build = store.state.join("build");
        for t in ["wsp-w2x", "wsp-w2y", "wsp-w2z", "my-wsp-w2q"] {
            std::fs::create_dir_all(build.join(t).join("tree")).unwrap();
        }
        // `w2y` is alive and `wsp-w2z` is the caller's own, which is the second
        // exclusion and the one a preview computed beside the act would be free
        // to disagree about.
        let live = vec!["w2y".to_string()];
        let mine = build.join("wsp-w2z");

        let said = stale_build_dirs(&store, Some(&live), &mine);
        let said_names: Vec<String> = said.iter().map(|d| util::contract(d)).collect();
        assert_eq!(said.len(), 2, "the preview named the wrong trees: {said_names:?}");
        assert!(said.iter().all(|d| d.exists()), "a dry run removed a tree");
        assert!(build.join("wsp-w2x").exists() && build.join("my-wsp-w2q").exists());

        let did = clear_build_dirs(&store, Some(&live), &mine);
        assert_eq!(did, said_names, "the removal disagreed with the preview");
        assert!(build.join("wsp-w2y").exists(), "a live agent lost the tree it was building in");
        assert!(mine.exists(), "the caller's own tree went with the residue");

        // Sorted, and the same order both times: the preview and the act print
        // the same list, so a stable order is part of them agreeing rather than
        // a nicety. A directory listing has whatever order the filesystem felt
        // like on the day.
        let mut expected = said_names.clone();
        expected.sort();
        assert_eq!(said_names, expected, "the preview printed a directory listing in filesystem order");

        // And backend's silence reaches the preview as it reaches the act:
        // nothing named, because nothing would go. The caller is told which of
        // the two it got by `verify` rather than by this, which is why the
        // empty list is not enough on its own.
        assert!(
            stale_build_dirs(&store, None, &mine).is_empty(),
            "silence from backend was previewed as a machine with no agents on it"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// The warm half of the same promise, and the half where the preview has to
    /// *take* something to be truthful.
    ///
    /// [`free_warm`] answers "which trees would go" by claiming them, because
    /// that is the question [`clear_warm`] keys on: a tree somebody is building
    /// in cannot be claimed and is therefore not removed. A preview that read
    /// the lock files instead would be a second implementation of the claim and
    /// would differ from it on a lock left behind by a build that was killed —
    /// held to a reader, free to the act.
    ///
    /// Two slots have trees and the third has only a lock, which is the state
    /// `claim_at` documents and the one a preview gets wrong by naming three.
    #[test]
    fn a_dry_run_of_rm_all_names_the_warm_trees_the_removal_then_takes() {
        let dir = scratch_dir("foresee-warm");
        repo(&dir);
        let store = Store::at(dir.join("store"), dir.join("state"));
        let warm = store.state.join("warm");
        let named = util::slugify(dir.file_name().unwrap().to_str().unwrap());
        std::fs::create_dir_all(&warm).unwrap();
        for slot in [0, 1] {
            std::fs::create_dir_all(warm.join(format!("{named}-{slot}")).join("tree")).unwrap();
        }
        // Slot 2 is a claim with no tree behind it: `claim_at` makes the lock
        // and whoever builds there makes the directory.
        assert!(!warm.join(format!("{named}-2")).exists());

        let said: Vec<String> =
            free_warm(&store, sharing::Live::Pids(&alive), &dir, Path::new("/nowhere"), true).iter().map(|w| util::contract(&w.dir)).collect();
        assert_eq!(said.len(), 2, "a slot nothing has built in was named as a tree: {said:?}");
        assert!(warm.join(format!("{named}-0")).exists(), "a dry run removed a warm tree");
        assert!(warm.join(format!("{named}-1")).exists(), "a dry run removed a warm tree");

        // The claims are dropped with the `Warm`s above, so the removal can
        // take them: a preview that held its locks past the line it printed
        // would make the next real `--rm` a no-op.
        let did = clear_warm(&store, sharing::Live::Pids(&alive), &dir, &dir, Path::new("/nowhere"), true);
        assert_eq!(did, said.len(), "the removal disagreed with the preview");
        assert!(!warm.join(format!("{named}-0")).exists(), "the tree the preview named was left standing");
        assert!(!warm.join(format!("{named}-1")).exists(), "the tree the preview named was left standing");
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// A defect the split above uncovered rather than one it introduced, and
    /// the exact thing a dry run is for: **`--rm --all` was skipping the
    /// caller's own warm tree** — the one `--rm` alone removes.
    ///
    /// The old shape computed the narrow answer first, unconditionally: it
    /// claimed the tree the last build wrote down, *held that claim*, and then
    /// under `--all` asked [`crate::sharing::warm_each`] for every free tree.
    /// The lock it was still holding is a lock, and the pid inside it is this
    /// process, which `ps` reports as alive — so the claim it had just taken
    /// made its own tree read as busy, and the widest removal in wsp left
    /// standing the one tree the caller most certainly meant.
    ///
    /// Silent both ways: `--all` printed a count that was right about the trees
    /// it did take, and nobody had a list to hold it against. It is asserted
    /// here on the preview because the preview is now where such a list exists.
    #[test]
    fn rm_all_takes_the_tree_the_caller_last_built_in_and_not_only_the_others() {
        let dir = scratch_dir("foresee-mine");
        let store = Store::at(dir.join("store"), dir.join("state"));
        let warm = store.state.join("warm");
        let named = util::slugify(dir.file_name().unwrap().to_str().unwrap());
        for slot in [0, 1, 2] {
            std::fs::create_dir_all(warm.join(format!("{named}-{slot}")).join("tree")).unwrap();
        }
        // The pointer a build leaves behind, naming slot 1 as the tree this
        // caller last built in — which is what `--rm` on its own would take.
        let scratch = dir.join("scratch");
        std::fs::create_dir_all(&scratch).unwrap();
        std::fs::write(
            scratch.join(BUILT_AT),
            format!("{}\n", warm.join(format!("{named}-1")).join("target").display()),
        )
        .unwrap();

        let narrow: Vec<String> =
            free_warm(&store, sharing::Live::Pids(&alive), &dir, &scratch, false).iter().map(|w| util::contract(&w.dir)).collect();
        assert_eq!(narrow.len(), 1, "the pointer did not name one tree: {narrow:?}");

        // The mechanism, reproduced rather than argued: hold the narrow claim
        // across the wide question, which is exactly what the old shape did to
        // itself, and the wide answer comes back short of the tree being held.
        let held = free_warm(&store, sharing::Live::Pids(&alive), &dir, &scratch, false);
        let while_held: Vec<String> =
            free_warm(&store, sharing::Live::Pids(&alive), &dir, &scratch, true).iter().map(|w| util::contract(&w.dir)).collect();
        assert_eq!(while_held.len(), 2, "a held claim did not take its slot out of the wide answer");
        assert!(!while_held.contains(&narrow[0]), "the held tree was claimed twice at once");
        drop(held);

        let wide: Vec<String> =
            free_warm(&store, sharing::Live::Pids(&alive), &dir, &scratch, true).iter().map(|w| util::contract(&w.dir)).collect();
        assert_eq!(wide.len(), 3, "--all did not reach every free tree: {wide:?}");
        assert!(
            wide.contains(&narrow[0]),
            "--all left standing the one tree --rm on its own removes: {wide:?} against {narrow:?}"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }
    /// Everything this command was failing to say, said. A run that goes red on
    /// this suite is usually the only run that will — the next one is green —
    /// so the name and the assertion have to come out of the output that
    /// already exists rather than out of a second run that no longer fails.
    ///
    /// The fixture is a real `cargo test --quiet` run, verbatim, because what
    /// is being parsed is somebody else's format and a fixture we wrote to suit
    /// the parser would prove only that the parser matches itself.
    #[test]
    fn a_red_run_names_the_test_and_what_its_assertion_said() {
        let out = "\nrunning 2 tests\n. 1/2\nt::a_thing_that_does_not_hold --- FAILED\n\nfailures:\n\n---- t::a_thing_that_does_not_hold stdout ----\n\nthread 't::a_thing_that_does_not_hold' (104182998) panicked at src/main.rs:7:39:\nassertion `left == right` failed: the counts differ\n  left: 1\n right: 2\nnote: run with `RUST_BACKTRACE=1` environment variable to display a backtrace\n\n\nfailures:\n    t::a_thing_that_does_not_hold\n\ntest result: FAILED. 1 passed; 1 failed; 0 ignored; 0 measured; 0 filtered out; finished in 0.00s\n\nerror: test failed, to rerun pass `--bin q`\n";

        let f = failures(out);
        assert_eq!(f.len(), 1, "the failing test was not found: {f:?}");
        assert_eq!(f[0].name, "t::a_thing_that_does_not_hold");
        assert_eq!(f[0].at.as_deref(), Some("src/main.rs:7:39"));
        assert_eq!(
            f[0].message.as_deref(),
            Some("assertion `left == right` failed: the counts differ"),
            "the assertion was dropped and only the name kept"
        );
    }

    /// The list is the authoritative set and the blocks are not: a harness that
    /// prints one and shortens the other still has to leave a name behind. A
    /// failure reported by name alone is worth having — it is the thing you
    /// cannot get back from a green re-run.
    #[test]
    fn a_failure_with_no_detail_block_is_still_named() {
        let out = "\nfailures:\n    panel::keys::a_key_that_moves\n    store::an_id_is_never_reissued\n\ntest result: FAILED. 480 passed; 2 failed; 0 ignored\n";
        let f = failures(out);
        assert_eq!(
            f.iter().map(|x| x.name.as_str()).collect::<Vec<_>>(),
            vec!["panel::keys::a_key_that_moves", "store::an_id_is_never_reissued"]
        );
        assert!(f[0].message.is_none(), "a message was invented for a test that printed none");
    }

    /// A compile failure names no test, and must not appear to. Cargo has
    /// already printed the error in full; a fabricated test path beside it
    /// would be worse than the silence this replaces.
    #[test]
    fn a_build_that_did_not_compile_invents_no_test_name() {
        let out = "error[E0425]: cannot find value `rows` in this scope\n  --> src/panel/rows.rs:12:9\nerror: could not compile `wsp` (bin \"wsp\") due to 1 previous error\n";
        assert!(failures(out).is_empty(), "a compile error was reported as a failing test");
    }

    /// The re-run alone is a measurement, so it declines to guess. A doc test
    /// is named `src/store.rs - Store::at (line 12)`, which libtest cannot be
    /// handed as an exact filter — handing it over anyway would run the whole
    /// suite again under the name of one test, which is the retry this command
    /// exists not to do.
    #[test]
    fn a_name_libtest_cannot_filter_on_is_not_rerun_at_all() {
        let nowhere = Path::new("/nowhere");
        let share = sharing::unregistered();
        let runs = inproc_runs(&scratch_dir("rerun-name"));
        assert_eq!(
            rerun_alone(&runs, nowhere, nowhere, "src/store.rs - Store::at (line 12)", &share),
            None
        );
        assert_eq!(rerun_alone(&runs, nowhere, nowhere, "", &share), None);
    }
}

/// verify under `WSP_JOBS=manager` (wsp-054): its cargo runs, test listing and
/// `--alone` runs are jobs on a manager on loopback, and a warm tree's lock is
/// read from that manager's job table. Every job is a `sh` script or `cargo
/// --version`-sized, under the test's own tree root.
#[cfg(test)]
mod jobs_tests {
    use super::*;
    use std::net::{SocketAddr, TcpListener};
    use wsp_host_api::{Role, Token};
    use wsp_manager::{Config, Manager, TOKEN_FILE};
    use wsp_wire::host::{Host, JobClass, JobState, Tcp};

    /// The host `place_of` names a row's tree on: this machine, until rows
    /// record their seat's host. The manager must be it, or it refuses the
    /// place as another host's.
    fn host() -> String {
        host_of(Whose::Row("any"))
    }

    /// Jobs placed under this test's tree root, on this thread.
    fn rooted(place: &Place) {
        crate::job_place::set_tree_root(Some(place.root.clone()));
    }

    /// A tree root with a `tree` and a `target` in it, an exec dir and a
    /// state dir beside it.
    struct Place {
        top: PathBuf,
        root: PathBuf,
        /// A place another process made, left for it to remove.
        borrowed: bool,
    }

    impl Place {
        fn new(tag: &str) -> Place {
            let base = std::env::var_os("WSP_SCRATCH").map(PathBuf::from).unwrap_or_else(std::env::temp_dir);
            let top = base.join(format!("wvj-{tag}-{}", std::process::id()));
            let _ = std::fs::remove_dir_all(&top);
            let root = top.join("trees");
            std::fs::create_dir_all(root.join("w").join("tree")).unwrap();
            std::fs::create_dir_all(root.join("w").join("target")).unwrap();
            std::fs::create_dir_all(top.join("state")).unwrap();
            Place { top, root, borrowed: false }
        }
        fn tree(&self) -> PathBuf {
            self.root.join("w").join("tree")
        }
        fn state(&self) -> PathBuf {
            self.top.join("state")
        }
    }

    impl Drop for Place {
        fn drop(&mut self) {
            if !self.borrowed {
                let _ = std::fs::remove_dir_all(&self.top);
            }
        }
    }

    fn free_port() -> u16 {
        TcpListener::bind("127.0.0.1:0").unwrap().local_addr().unwrap().port()
    }

    /// A manager on loopback whose tree root is the place's, and a client
    /// for it as wsp dials one.
    fn manager(place: &Place) -> (Manager, Tcp) {
        let (m, port, token) = manager_at(place);
        (m, client(port, &token))
    }

    fn client(port: u16, token: &str) -> Tcp {
        Tcp::new(SocketAddr::from(([127, 0, 0, 1], port)), Token(token.into()), Role::Cli, "test")
    }

    /// The manager alone, and the port and token a client dials it with.
    fn manager_at(place: &Place) -> (Manager, u16, String) {
        let exec = place.top.join("exec");
        let mut cfg = Config::new(&exec, &host());
        cfg.port = free_port();
        cfg.tree_root = place.root.clone();
        cfg.job_grace = Duration::from_millis(500);
        let since = Instant::now();
        let m = loop {
            match Manager::start(cfg.clone()) {
                Ok(m) => break m,
                Err(wsp_manager::StartError::Locked { .. }) if since.elapsed() < Duration::from_secs(2) => {
                    std::thread::sleep(Duration::from_millis(5))
                }
                Err(e) => panic!("the manager starts on loopback: {e}"),
            }
        };
        let token = std::fs::read_to_string(exec.join(TOKEN_FILE)).unwrap().trim().to_string();
        (m, cfg.port, token)
    }

    fn run(row: &str, mark: &str) -> sharing::Run {
        sharing::Run { host: host(), row: row.into(), attempt: 0, mark: mark.into() }
    }

    fn within(wait: Duration, mut f: impl FnMut() -> bool) -> bool {
        let until = Instant::now() + wait;
        while Instant::now() < until {
            if f() {
                return true;
            }
            std::thread::sleep(Duration::from_millis(25));
        }
        f()
    }

    /// The job rows of this run, as `(job, class, state)`.
    fn rows_of(host: &dyn Host, run: &sharing::Run) -> Vec<(String, JobClass, JobState)> {
        let mut rows: Vec<_> = host
            .jobs()
            .unwrap()
            .into_iter()
            .filter(|r| run.owns(&r.key.row.0, r.key.attempt, &r.key.job))
            .map(|r| (r.key.job, r.class, r.state))
            .collect();
        rows.sort_by(|a, b| a.0.cmp(&b.0));
        rows
    }

    /// A verify job takes its place from `place_of(row, verify)`: a
    /// `HostDir` naming the row's host and a path relative to that host's
    /// tree root, never a path on wsp's machine; and the job runs there. A
    /// tree outside the root is no place.
    #[test]
    fn a_verify_job_takes_its_place_from_place_of_and_names_the_rows_host() {
        let place = Place::new("place");
        rooted(&place);
        let (m, tcp) = manager(&place);
        let p = place_of(Whose::Row("wsp-054"), TreeKind::Verify, &place.tree()).unwrap();
        assert_eq!(p.host(), host(), "the row's host");
        match &p.at {
            wsp_wire::host::JobAt::Dir(d) => {
                assert_eq!(d.path, "w/tree", "relative to the host's tree root");
                assert!(!d.escapes());
            }
            other => panic!("expected a HostDir, got {other:?}"),
        }
        let me = run("wsp-054", "p1");
        assert_eq!(me.host, p.host(), "the run's locks are read from the host its jobs run on");
        let jobs = Jobs::new(&tcp, me);
        let (ended, out) = jobs
            .run("pwd", JobClass::Check, &place.tree(), &[], vec!["pwd".into()], Duration::from_secs(30), &mut |_| {})
            .unwrap();
        assert_eq!(ended.exit, Some(0), "{out}");
        assert_eq!(PathBuf::from(out.trim()).canonicalize().unwrap(), place.tree().canonicalize().unwrap(), "it ran in the place");

        let outside = place.top.join("elsewhere");
        std::fs::create_dir_all(&outside).unwrap();
        assert!(place_of(Whose::Row("wsp-054"), TreeKind::Verify, &outside).is_err(), "a tree outside the root is no place");
        assert!(jobs.run("pwd", JobClass::Check, &outside, &[], vec!["pwd".into()], Duration::from_secs(30), &mut |_| {}).is_err());
        assert_eq!(relative(&place.tree(), &place.root.join("w").join("target")), PathBuf::from("../target"));
        m.stop();
    }

    /// The build is one `job.run` of class `build`: `CARGO_TARGET_DIR` and
    /// the share's `CARGO_BUILD_JOBS` cross as the env prefix (cargo reads
    /// the target back), and its output comes back from the job.
    #[test]
    fn verifys_cargo_is_a_build_job_with_its_target_and_share_in_the_env_prefix() {
        let place = Place::new("cargo");
        rooted(&place);
        let tree = place.tree();
        std::fs::write(tree.join("Cargo.toml"), "[package]\nname = \"t\"\nversion = \"0.1.0\"\nedition = \"2021\"\n\n[workspace]\n").unwrap();
        std::fs::create_dir_all(tree.join("src")).unwrap();
        std::fs::write(tree.join("src/lib.rs"), "").unwrap();
        let (m, tcp) = manager(&place);
        let me = run("wsp-054", "c1");
        let runs = Jobs::new(&tcp, me.clone());
        let share = sharing::unregistered();
        let target = place.root.join("w").join("target");
        let (ok, text) =
            cargo_job(&runs, &tree, &target, &["metadata", "--no-deps", "--format-version", "1", "--offline"], false, &share);
        assert!(ok, "{text}");
        let meta: serde_json::Value = serde_json::from_str(text.lines().find(|l| l.starts_with('{')).unwrap()).unwrap();
        let said = PathBuf::from(meta["target_directory"].as_str().unwrap());
        assert_eq!(said.canonicalize().unwrap(), target.canonicalize().unwrap(), "CARGO_TARGET_DIR crossed");
        let rows = rows_of(&tcp, &me);
        assert_eq!(rows.len(), 1, "one job: {rows:?}");
        assert!(rows[0].0.starts_with("verify:cargo-metadata:0@"), "{rows:?}");
        assert_eq!(rows[0].1, JobClass::Build);

        // The share's job count is in the env, as `apply` puts it on a child.
        let jobs = &runs;
        let (k, v) = share.env();
        let (ended, out) = jobs
            .run("env", JobClass::Build, &tree, &[(k.into(), v)], vec!["sh".into(), "-c".into(), "echo jobs=$CARGO_BUILD_JOBS idx=${GIT_INDEX_FILE-unset}".into()], Duration::from_secs(30), &mut |_| {})
            .unwrap();
        assert_eq!(ended.exit, Some(0));
        assert!(out.contains("jobs=1 idx=unset"), "{out}");
        m.stop();
    }

    /// The build's output is handed on while the job runs, not when it ends:
    /// the job waits for a file the echo of its first line makes, so output
    /// held back to the end would leave it to time out.
    #[test]
    fn a_build_jobs_output_echoes_while_it_runs() {
        let place = Place::new("echo");
        let (m, tcp) = manager(&place);
        echo_flow(&place, &tcp);
        m.stop();
    }

    fn echo_flow(place: &Place, tcp: &Tcp) {
        rooted(place);
        let me = run("wsp-054", "e1");
        let jobs = Jobs::new(tcp, me.clone());
        let go = place.tree().join("go");
        let script = "echo first; i=0; while [ ! -e go ] && [ $i -lt 200 ]; do sleep 0.05; i=$((i+1)); done; [ -e go ] && echo second";
        let mut heard = Vec::new();
        let (ended, text) = jobs
            .run("cargo-test", JobClass::Build, &place.tree(), &[], vec!["sh".into(), "-c".into(), script.into()], Duration::from_secs(30), &mut |t| {
                heard.push(t.to_string());
                if t.contains("first") {
                    std::fs::write(&go, "").unwrap();
                }
            })
            .unwrap();
        assert_eq!(ended.exit, Some(0), "the job saw the echo before it ended: {text:?}");
        assert!(text.contains("first") && text.contains("second"), "{text}");
        assert!(heard.concat().contains("second"));
    }

    /// A warm tree whose build was killed is free by the job table, with no
    /// `kill -0`: held while its build job runs, free once the job ends, even
    /// though the killed verify never removed its lock. A run that never ran
    /// a job holds its claim through setup.
    #[test]
    fn a_lock_left_by_a_killed_build_is_free_by_the_job_table() {
        let place = Place::new("lock");
        let (m, tcp) = manager(&place);
        lock_flow(&place, &tcp);
        m.stop();
    }

    fn lock_flow(place: &Place, tcp: &Tcp) {
        rooted(place);
        let repo = place.top.join("repo");
        let a = run("wsp-054", "a1");
        let b = run("wsp-099", "b1");
        let jobs_a = Jobs::new(tcp, a.clone());
        let jobs_b = Jobs::new(tcp, b.clone());
        let table_a = |h: &str| jobs_a.table(h);
        let table_b = |h: &str| jobs_b.table(h);
        let live_a = sharing::Live::Jobs { me: &a, table: &table_a };
        let live_b = sharing::Live::Jobs { me: &b, table: &table_b };

        let warm = sharing::warm(&place.state(), &repo, 1, live_a).expect("a free tree");
        assert!(warm.dir.starts_with(place.state().join("warm").join(crate::util::slugify(&host()))), "warm trees are per host: {:?}", warm.dir);
        assert!(sharing::warm(&place.state(), &repo, 1, live_b).is_none(), "held through setup, with no job yet");
        assert!(warm.touch());

        std::thread::scope(|s| {
            let build = s.spawn(|| {
                rooted(place);
                jobs_a.run("cargo-test", JobClass::Build, &place.tree(), &[], vec!["sh".into(), "-c".into(), "sleep 30".into()], Duration::from_secs(60), &mut |_| {})
            });
            assert!(
                within(Duration::from_secs(10), || rows_of(tcp, &a).iter().any(|r| r.2 == JobState::Running)),
                "the build job runs"
            );
            assert!(sharing::warm(&place.state(), &repo, 1, live_b).is_none(), "held while its build runs");
            // The build is killed, and its verify with it: the lock stays.
            let key = tcp.jobs().unwrap().into_iter().find(|r| a.owns(&r.key.row.0, r.key.attempt, &r.key.job)).unwrap().key;
            tcp.stop_job(&key, Duration::from_secs(10)).expect("the manager ends it");
            let (ended, _) = build.join().unwrap().unwrap();
            assert_ne!(ended.exit, Some(0), "{ended:?}");
        });
        let lock = warm.dir.with_extension("lock");
        std::mem::forget(warm);
        assert!(lock.exists(), "the killed verify left its lock");
        let again = sharing::warm(&place.state(), &repo, 1, live_b).expect("free by the job table");
        assert!(again.dir.ends_with(format!("{}-0", crate::util::slugify("repo"))));
        assert!(std::fs::read_to_string(&lock).unwrap().contains("wsp-099"), "the lock is the new run's");
        assert!(again.touch(), "and the new run holds it");
    }

    /// `--alone`'s listing and per-test runs are jobs of class `check`: a
    /// pass, a red test read from the job's log, and a test past its budget
    /// ended by the manager and reported as the direct path reports it.
    #[test]
    fn alones_listing_and_runs_are_check_jobs() {
        let place = Place::new("alone");
        let (m, tcp) = manager(&place);
        alone_flow(&place, &tcp);
        m.stop();
    }

    fn alone_flow(place: &Place, tcp: &Tcp) {
        rooted(place);
        let me = run("wsp-054", "al1");
        let runs = Jobs::new(tcp, me.clone());
        let tree = place.tree();
        let bin = place.root.join("w").join("target");
        let lists = bin.join("lists");
        wsp_testkit::script(&lists, "#!/bin/sh\nprintf 'a::b: test\\nc::d: test\\nx: benchmark\\n\\n2 tests\\n'\n");
        assert_eq!(test_names(&runs, &tree, &lists), vec!["a::b".to_string(), "c::d".to_string()]);

        let ok = bin.join("passes");
        wsp_testkit::script(&ok, "#!/bin/sh\nexit 0\n");
        assert!(run_alone(&runs, &tree, &ok, "a::b", Some(Duration::from_secs(30))).is_none());

        let red = bin.join("fails");
        wsp_testkit::script(
            &red,
            "#!/bin/sh\ncat <<'OUT'\nrunning 1 test\ntest a::b ... FAILED\n\nfailures:\n\n---- a::b stdout ----\nthread 'a::b' panicked at src/a.rs:12:9:\nthe seat was empty\n\nfailures:\n    a::b\n\ntest result: FAILED. 0 passed; 1 failed\nOUT\nexit 101\n",
        );
        let f = run_alone(&runs, &tree, &red, "a::b", Some(Duration::from_secs(30))).expect("red");
        assert_eq!(f.at.as_deref(), Some("src/a.rs:12:9"));
        assert_eq!(f.message.as_deref(), Some("the seat was empty"));

        let hangs = bin.join("hangs");
        wsp_testkit::script(&hangs, "#!/bin/sh\nexec sleep 300\n");
        let f = run_alone(&runs, &tree, &hangs, "a::b", Some(Duration::from_secs(1))).expect("a hang is a failure");
        assert!(f.message.as_deref().unwrap_or_default().contains("no answer"), "{f:?}");

        let rows = rows_of(tcp, &me);
        assert_eq!(rows.len(), 4, "{rows:?}");
        assert!(rows.iter().all(|r| r.1 == JobClass::Check), "{rows:?}");
        assert_eq!(rows.iter().filter(|r| r.0.starts_with("verify:alone:")).count(), 3, "{rows:?}");
        assert_eq!(rows.iter().filter(|r| r.0.starts_with("verify:list:")).count(), 1, "{rows:?}");
    }

    /// The child's half of `under_manager_wsp_runs_no_ps_and_no_kill`: the
    /// flows above in a process of its own, so the call log it prints holds
    /// what they ran and nothing a test beside them did.
    #[test]
    #[ignore = "run by under_manager_wsp_runs_no_ps_and_no_kill, in a process of its own"]
    fn manager_flows_for_the_call_log() {
        let Ok(said) = std::env::var("WSP_TEST_CALL_LOG") else { return };
        let parts: Vec<&str> = said.split('\t').collect();
        let place = Place { top: PathBuf::from(parts[2]), root: PathBuf::from(parts[3]), borrowed: true };
        let tcp = client(parts[0].parse().unwrap(), parts[1]);
        let _ = wsp_base::calls::drain_children();
        lock_flow(&place, &tcp);
        alone_flow(&place, &tcp);
        echo_flow(&place, &tcp);
        println!("\nCALLS {}", wsp_base::calls::drain_children().json());
    }

    /// Under `manager` the call log of verify's flows holds no `ps` and no
    /// `kill` from wsp: a killed build is read free from the job table, and
    /// a test past its budget is ended by the manager. The manager runs here,
    /// in this process, as it runs in a process of its own in use; what it
    /// spawns and signals is its own and not in the child's log.
    #[test]
    fn under_manager_wsp_runs_no_ps_and_no_kill() {
        let place = Place::new("calls");
        let (m, port, token) = manager_at(&place);
        let said = format!("{port}\t{token}\t{}\t{}", place.top.display(), place.root.display());
        let out = std::process::Command::new(std::env::current_exe().unwrap())
            .args(["--exact", "cmd_verify::jobs_tests::manager_flows_for_the_call_log"])
            .args(["--ignored", "--nocapture", "--test-threads=1"])
            .env("WSP_TEST_CALL_LOG", said)
            .output()
            .unwrap();
        m.stop();
        let said = format!("{}{}", String::from_utf8_lossy(&out.stdout), String::from_utf8_lossy(&out.stderr));
        assert!(out.status.success(), "the child failed:\n{said}");
        let line = said.lines().find_map(|l| l.strip_prefix("CALLS ")).unwrap_or_else(|| panic!("no call log:\n{said}"));
        let calls: serde_json::Value = serde_json::from_str(line).unwrap();
        assert!(calls.get("sh").is_none() && calls.get("cargo").is_none(), "the jobs ran in this process: {line}");
        for program in ["ps", "kill", "lsof"] {
            assert!(calls.get(program).is_none(), "wsp ran {program}: {line}");
        }
    }

    /// A verify in a checkout is keyed by its row; one outside any row by the
    /// agent's workspace, and two runs never share a mark.
    #[test]
    fn a_verify_with_no_row_is_keyed_by_its_workspace_and_each_run_by_its_mark() {
        let in_row = this_run(Some(Path::new("/x/.worktrees/wsp-054")), "cpd-185");
        assert_eq!(in_row.row, "wsp-054");
        assert_eq!(in_row.host, host(), "the row's host, as place_of names it");
        let bare = this_run(None, "cpd-185");
        assert_eq!(bare.row, "cpd-185", "the workspace stands in for the row");
        std::thread::sleep(Duration::from_millis(2));
        assert_ne!(this_run(None, "cpd-185").mark, bare.mark, "unique per run");
    }
}
