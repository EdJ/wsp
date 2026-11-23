//! A barrier check's tree, taken away once its check has gone: `wsp-289`.
//!
//! `wsp spawn <list> --check N` gives the check a tree of its own,
//! `<trunk>/.worktrees/<list>-check-<N>` ([`crate::barrier::subject`]), so its
//! builds are nobody else's. Ending the seat was meant to take the tree with
//! it, and never did: the ending is launched by the `wsp worklist go` the
//! check types from inside that tree, every process under it inherited the
//! cwd, and `despawn` read itself as somebody standing in the tree and kept
//! it — saying so on a stdout nobody read. compound's disk reached 0 bytes
//! free on 2026-10-17 with 56 of them in `.worktrees/`, each with a 7-10 GB
//! `target/`, and none in use or changed.
//!
//! So two halves, with one rule between them:
//!
//! - `despawn` of a check's seat takes the tree by [`discard`], and the
//!   ending runs it from `/` (`cycle::despawn_seat`), so the only one standing
//!   in the tree is somebody else.
//! - The tick, and the launched ending after its despawn, run [`tidy`]: every
//!   project root's `.worktrees/` read off the disk, and every check tree in
//!   it whose check is no longer anybody's is taken. That is what reaches a
//!   tree whose seat went some other way, or before this, and it is
//!   idempotent because a removed tree is not there to find again.
//!
//! **The rule**: the check has settled and its seat is ended, no live pane
//! stands in the tree, and the tree holds nothing beyond build output — no
//! tracked change, nothing untracked, and nothing ignored but `target/` and the
//! other residue ([`Residue`]: `__pycache__/`, `*.pyc`, a `Cargo.lock` below the
//! root, and a project's own `## Residue` list; `wsp-327`). A check reads; it
//! has no business writing, and something written there is kept and named with
//! its path.
//!
//! **A verifier has no tree of its own.** `spawn <member> --verify` stands it
//! in its member's tree (`tree_name` is the task), which is the member's and
//! goes with the member's work; `<member>-verify` names its brief and nothing
//! on disk.

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::time::{Duration, Instant};

use crate::cmd_checkout::{self, Tree, WORKTREES};
use crate::cmd_verify::git;
use crate::host::{Host, LOCAL};
use crate::place::Place;
use crate::store::Store;
use crate::util;

/// The check a directory under `.worktrees/` is the tree of, if it is one.
///
/// A name is only a check's when it parses as one **and** the list it names
/// exists **and** no task has that id: `parse_subject` alone would read a
/// project called `x-check`'s task `x-check-012` as list `x`'s twelfth group.
pub fn named(store: &Store, name: &str) -> Option<(String, usize)> {
    let (list, at) = crate::util::parse_subject(name)?;
    if store.task(name).is_some() || store.worklist(list).is_none() {
        return None;
    }
    Some((list.to_string(), at))
}

/// Whether group `at` of `list` still has a check that is somebody's: one
/// still reading, or a seat not yet ended. A fresh check on the same group is
/// started in the same tree, and its entry is written before its seat exists.
pub fn in_use(store: &Store, list: &str, at: usize) -> bool {
    store
        .worklist(list)
        .and_then(|w| w.groups().into_iter().nth(at.saturating_sub(1)))
        .is_some_and(|g| g.checks.iter().any(|c| c.open() || c.standing().is_some()))
}

/// The heading a project lists extra residue globs under, one per bullet.
pub const RESIDUE_SECTION: &str = "Residue";

/// What a build leaves behind that git ignores and nobody wrote: dropping it
/// costs a rebuild and nothing else. A glob without a `/` (after a trailing one
/// is dropped) names a file or directory at any depth; one with a `/` is
/// anchored at the tree's root.
///
/// **Web trees too** (`wsp-373`): `node_modules/`, `dist/`, `test-results/`
/// and a dev server's `.dev-port`, `.dev.log`, `.dev/` and `.watch/`. A check
/// of a web project ran `npm ci` and a dev server in its tree; each of those
/// trees was kept as "more than build output" for a day, 5 GB apiece, because
/// none of this was on the list and only a project's own `## Residue` could
/// add it. Everything here is ignored by the project's own `.gitignore` or
/// would be named by it, and is rebuilt by the same command that made it.
const RESIDUE: &[&str] = &[
    "target/",
    "__pycache__/",
    "*.pyc",
    "node_modules/",
    "dist/",
    "test-results/",
    ".dev-port",
    ".dev.log",
    ".dev/",
    ".watch/",
];

/// The ignored paths a tree may hold and still count as having nothing in it.
/// Only an ignored path is ever residue: an untracked one is somebody's.
pub struct Residue {
    extra: Vec<String>,
}

impl Residue {
    /// The built-in list and every `## Residue` bullet of a project rooted in `repo`.
    pub fn of(store: &Store, repo: &Path) -> Residue {
        let here = util::real(&repo.display().to_string());
        let extra = store
            .projects()
            .iter()
            .filter(|p| p.roots.iter().any(|r| repo_of(r).is_some_and(|d| util::real(&d.display().to_string()) == here)))
            .filter_map(|p| p.section(RESIDUE_SECTION))
            .flat_map(|s| s.lines().filter_map(glob_of).collect::<Vec<_>>())
            .collect();
        Residue { extra }
    }

    /// Whether the ignored `path`, as `git status` reports it, is residue.
    fn covers(&self, path: &str) -> bool {
        let path = path.trim_end_matches('/');
        let segments: Vec<&str> = path.split('/').collect();
        if segments.len() > 1 && segments.last() == Some(&"Cargo.lock") {
            return true;
        }
        RESIDUE.iter().copied().chain(self.extra.iter().map(String::as_str)).any(|glob| {
            let glob = glob.trim_end_matches('/');
            match glob.contains('/') {
                false => segments.iter().any(|s| crate::util::captures(glob, s).is_some()),
                true => (1..=segments.len()).any(|n| crate::util::captures(glob, &segments[..n].join("/")).is_some()),
            }
        })
    }
}

fn glob_of(line: &str) -> Option<String> {
    let bullet = line.trim().strip_prefix("- ").or_else(|| line.trim().strip_prefix("* "))?;
    let glob = bullet.trim().trim_start_matches('`').split(['`', ' ']).next()?;
    (!glob.is_empty()).then(|| glob.to_string())
}

/// What is in a tree besides build output, by path — `None` when git cannot
/// read it, which keeps it. A top-level `target` is never counted; an ignored
/// path is not counted when `residue` covers it.
pub fn litter(dir: &Path, residue: &Residue) -> Option<Vec<String>> {
    let out = git(dir, &["status", "--porcelain", "--ignored=matching", "--untracked-files=normal"])?;
    Some(
        out.lines()
            .filter_map(|l| Some((l.get(..2)?, l.get(3..)?.trim_matches('"'))))
            .filter(|(_, p)| *p != "target" && !p.starts_with("target/"))
            .filter(|(code, p)| !(*code == "!!" && residue.covers(p)))
            .map(|(_, p)| p.to_string())
            .collect(),
    )
}

/// How long a tree found to hold more than build output is believed before
/// `git status` is asked about it again (`wsp-665`).
const LITTER_RECHECK: Duration = Duration::from_secs(600);

/// What `git status --ignored` found in each kept tree, and when. A kept tree is
/// found again every tick and its answer is a full status of a worktree (0.3 s
/// each on `compounding`'s 8,800 files, nine of them in a 200 s sample of the
/// daemon): `sweep:check-trees` was 3.6 s of an idle full tick and 5.9 s of a
/// busy one, almost all of it this. A tree somebody tidies by hand is taken at
/// the next look past [`LITTER_RECHECK`]; one that is clean is never in here, so
/// a finished check's tree still goes the tick it is found clean.
static LITTER: Mutex<BTreeMap<PathBuf, (Instant, Vec<String>)>> = Mutex::new(BTreeMap::new());

/// Whether this process is in the daemon's sweep, which is the only reader that
/// believes a remembered answer: a verb, a `despawn` or a test that asks about a
/// tree wants what is in it now.
static BELIEVING: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

/// Run `f` (the tick's sweep of kept trees) with [`litter_lately`] remembering.
pub fn believing<T>(f: impl FnOnce() -> T) -> T {
    BELIEVING.store(true, std::sync::atomic::Ordering::SeqCst);
    let out = f();
    BELIEVING.store(false, std::sync::atomic::Ordering::SeqCst);
    out
}

/// [`litter`], believed for [`LITTER_RECHECK`] when it found something and the
/// daemon's sweep is asking.
pub fn litter_lately(dir: &Path, residue: &Residue) -> Option<Vec<String>> {
    match BELIEVING.load(std::sync::atomic::Ordering::SeqCst) {
        true => litter_at(dir, Instant::now(), || litter(dir, residue)),
        false => litter(dir, residue),
    }
}

fn litter_at(dir: &Path, now: Instant, ask: impl FnOnce() -> Option<Vec<String>>) -> Option<Vec<String>> {
    let mut seen = LITTER.lock().unwrap_or_else(|e| e.into_inner());
    if let Some((at, found)) = seen.get(dir) {
        if now.saturating_duration_since(*at) < LITTER_RECHECK {
            return Some(found.clone());
        }
    }
    seen.remove(dir);
    let found = ask();
    if let Some(f) = found.as_ref().filter(|f| !f.is_empty()) {
        seen.insert(dir.to_path_buf(), (now, f.clone()));
    }
    found
}

/// Why a tree is kept for what [`litter`] found in it.
pub fn holds_more(found: &[String]) -> String {
    let shown = found.iter().take(3).cloned().collect::<Vec<_>>().join(", ");
    let more = if found.len() > 3 { format!(" and {} more", found.len() - 3) } else { String::new() };
    format!("it holds more than build output: {shown}{more}")
}

// ---- wsp-691's agent-leftover stop -----------------------------------------
//
// What an agent's shell leaves in its tree is no job, so the stop below reads
// working directories ([`Host::cwds`]) and parent links ([`parents`]) through
// the host. wsp-057 took both out of the host with tree work's pid probes; the
// merge of Ed's master (wsp-368) kept wsp-691 whole on the person's word and
// put `cwds` back on the trait. wsp-389 has the manager stop a seat's process
// tree itself.

/// Every process as `(pid, ppid, "")`: the parent links alone, read through
/// the host's process table, for a caller that only walks
/// [`crate::base::procs::ancestors`].
pub fn parents() -> Vec<(u32, u32, String)> {
    LOCAL
        .table("pid=,ppid=", false)
        .unwrap_or_default()
        .lines()
        .filter_map(|l| {
            let mut it = l.split_whitespace().map(|x| x.parse::<u32>().ok());
            Some((it.next()??, it.next()??, String::new()))
        })
        .collect()
}

/// The pids in `lsof -d cwd -Fpn` output whose working directory is `dir` or
/// below it — the one fact that proves a process belongs to a tree.
pub fn pids_in(lsof: &str, dir: &Path) -> Vec<u32> {
    let mut out = Vec::new();
    let mut pid = None;
    for line in lsof.lines() {
        match line.split_at_checked(1) {
            Some(("p", rest)) => pid = rest.trim().parse::<u32>().ok(),
            Some(("n", rest)) => {
                if let (Some(p), true) = (pid, Path::new(rest).starts_with(dir)) {
                    if !out.contains(&p) {
                        out.push(p);
                    }
                }
            }
            _ => {}
        }
    }
    out
}

/// The pids in `lsof` output that are `dir`'s to stop: those with their working
/// directory in it, less `keep`.
///
/// **`keep` is every live seat's process tree, and the reason is
/// `wsp-388`.** A working directory proves a process was started in a tree, not
/// that it belongs to the check: `compound-sup` took the cwd of whoever ran
/// `wsp spawn` and held it, and a check's `wsp worklist go` ran inside its tree,
/// so the members and governors it seated stood in the tree by cwd while
/// being other seats. Ending the check signalled them (the `TERM`, 1.5s, `KILL`
/// below is the two seconds between the check going and the others).
pub fn strays(lsof: &str, dir: &Path, keep: &[u32]) -> Vec<u32> {
    pids_in(lsof, dir).into_iter().filter(|p| !keep.contains(p)).collect()
}

/// The backends of `all` that answer on this machine now.
fn here_of(all: &[Box<dyn Place>]) -> Vec<&dyn Place> {
    all.iter().map(|b| b.as_ref()).collect()
}

/// Send `sig` to `pids`, and say nothing of it: a process that has gone is the
/// outcome wanted.
pub fn signal(sig: &str, pids: &[u32]) {
    LOCAL.signal(pids, sig);
}

/// A finished row's tree that still has processes standing in it (`wsp-453`).
#[derive(Debug, PartialEq)]
pub struct Left {
    pub row: String,
    pub tree: String,
    /// What the tree started, and so the sweep stops.
    pub pids: Vec<u32>,
    /// What stands in the tree by working directory alone, with the reason it
    /// is let be (`wsp-478`).
    pub spared: Vec<(u32, String)>,
}

/// Hosts and servers: a process of one of these names is never the tree's to
/// stop, whatever its working directory (`wsp-478`: the backend server and a
/// `wsp` were TERMed for standing in a finished tree they were started from).
const HOSTS: &[&str] = &["wsp", "ssh", "sshd", "ssh-agent", "mosh-server", "tmux", "screen"];

/// A server a tree builds and runs itself: spared where it is installed, and
/// the tree's to stop where its executable lives under the tree
/// (`target/debug/compound-sup` left by a row).
const BUILT_SERVERS: &[&str] = &["compound-sup"];

/// The dev tools a row's tree runs: stopped by name, standing in the tree.
const DEV_TOOLS: &[&str] = &[
    "node", "vite", "vitest", "esbuild", "tsgo", "tsc", "npm", "npx", "pnpm", "yarn", "bun", "deno", "jest", "webpack",
    "playwright", "cargo", "rustc",
];

/// What the sweep does with one process that stands in a finished tree.
#[derive(Debug, PartialEq)]
pub enum Verdict {
    Stop,
    Spare(String),
}

/// Every process as `(pid, ppid, executable)`, from `ps -o comm=`.
pub fn exes() -> Vec<(u32, u32, String)> {
    let Some(out) = LOCAL.table("pid=,ppid=,comm=", false) else { return Vec::new() };
    out.lines()
        .filter_map(|l| {
            let (pid, rest) = l.trim_start().split_once(char::is_whitespace)?;
            let (ppid, rest) = rest.trim_start().split_once(char::is_whitespace)?;
            Some((pid.parse().ok()?, ppid.parse().ok()?, rest.trim().to_string()))
        })
        .collect()
}

fn base_of(exe: &str) -> &str {
    Path::new(exe).file_name().and_then(|n| n.to_str()).unwrap_or(exe)
}

/// Is a process with this executable a host the sweep never stops: a login
/// shell (`-zsh`) or a [`HOSTS`] name.
fn host_named(exe: &str) -> Option<String> {
    let b = base_of(exe);
    if b.starts_with('-') {
        return Some(format!("a login shell ({b})"));
    }
    HOSTS.contains(&b).then(|| format!("a host or server ({b})"))
}

/// Is this executable a person's launched app (`wsp-633`): inside a `.app`
/// bundle or under an `Applications` folder. A person's program, not an agent's.
fn app_named(exe: &str) -> Option<String> {
    let inside = exe.contains(".app/") || exe.contains("/Applications/");
    inside.then(|| format!("an app ({})", base_of(exe)))
}

/// The nearest app above `pid`, as `(its pid, why)`, climbing until a host
/// (whose children are the host's, and say so elsewhere) or the top.
fn app_above(pid: u32, table: &[(u32, u32, String)]) -> Option<(u32, String)> {
    let find = |p: u32| table.iter().find(|(q, _, _)| *q == p);
    let mut at = find(pid)?.1;
    for _ in 0..table.len() {
        if at <= 1 {
            break;
        }
        let &(_, up, ref exe) = find(at)?;
        if host_named(exe).is_some() {
            return None;
        }
        if let Some(why) = app_named(exe) {
            return Some((at, why));
        }
        at = up;
    }
    None
}

/// Does the executable live under `tree`? A relative one was run from the
/// working directory, which is in the tree.
fn exe_in(exe: &str, tree: &Path) -> bool {
    let p = Path::new(exe);
    (!p.is_absolute() && exe.contains('/')) || p.starts_with(tree)
}

/// The nearest host above `at` (itself included), when it is a `wsp` that
/// stands in the tree (`in_tree`): a wsp elsewhere, one a seat's agent ran the
/// test suite under, is not this tree's check.
fn wsp_above(mut at: u32, in_tree: &[u32], table: &[(u32, u32, String)]) -> Option<u32> {
    for _ in 0..table.len() {
        if at <= 1 {
            return None;
        }
        let &(_, up, ref exe) = table.iter().find(|(q, _, _)| *q == at)?;
        if host_named(exe).is_some() {
            return (base_of(exe) == "wsp" && in_tree.contains(&at)).then_some(at);
        }
        at = up;
    }
    None
}

/// The verbs whose `wsp` runs a row's checks: `wsp check` (a verifier's, the
/// barrier's, a seat's) and the verifier's `wsp verified`, which reruns them.
const CHECK_VERBS: &[&str] = &["check", "verified", "verify"];

/// The rows this command line's `wsp` runs checks on ([`CHECK_VERBS`]):
/// every word after the verb that is not a flag (`wsp check --at <sha> <row>`
/// names the sha too, which no tree is named after). Empty unless its first
/// word is a `wsp`, or its second where the first is the shell a `wsp` script
/// (a shim) runs under, and its first word after that not a flag is a check
/// verb.
fn checked_rows(command: &str) -> Vec<String> {
    let mut words = command.split_whitespace();
    let Some(first) = words.next() else { return Vec::new() };
    if base_of(first) != "wsp" && !(shell_named(first) && words.next().map(base_of) == Some("wsp")) {
        return Vec::new();
    }
    let mut args = words.filter(|w| !w.starts_with('-'));
    if !args.next().is_some_and(|verb| CHECK_VERBS.contains(&verb)) {
        return Vec::new();
    }
    args.map(str::to_string).collect()
}

/// How [`runners`] asks the command lines of some pids: [`commands`].
pub type Ask = dyn Fn(&[u32]) -> Vec<(u32, String)>;

/// What runs a check (`wsp-426`), as `(its pid, the row it is for)`: a `wsp`
/// in `table` ([`exes`]) whose command line ([`commands`], asked through `ask`)
/// runs a row's checks ([`checked_rows`]), wherever it stands (a `wsp` script
/// under its shell too); each running job of `jobs` (the manager's job table),
/// by its group's leader, the job's `pgid`, under its key's row; and every
/// job's leader whose own command line names its job ([`job_row`], `wsp-444`):
/// the check's own steps and the builds they send through the throttle. What
/// descends from one is that row's check's, never a leftover in its tree: the
/// verifier's `wsp check` runs from outside the tree, and its cargo was TERMed
/// with exit 143.
///
/// The leader's command line is what decides, whoever runs the job: a step a
/// manager runs descends from `wsp-manager`, not from the `wsp check` that
/// asked for it, and the daemon (`WSP_JOBS=inproc`) never asks the manager's
/// table, nor does a caller the manager does not answer within its second
/// (`wsp-444`: compound-react-058's `npm run check`, a manager job of the
/// `wsp check` its `wsp submit` started, was TERMed, exit 143). A step run in
/// a process (the daemon's, a `wsp review`'s or `wsp submit`'s, a check
/// seat's `wsp check`) has the same leader.
pub fn runners(table: &[(u32, u32, String)], ask: &Ask, jobs: &[wsp_host_api::JobRow]) -> Vec<(u32, String)> {
    let running = jobs.iter().filter(|j| j.state == wsp_host_api::JobState::Running);
    let mut out: Vec<(u32, String)> = running.map(|j| (j.pgid, j.key.row.0.clone())).collect();
    // A `wsp` script's executable is its shell where `ps` names the
    // interpreter, and a job's leader is the `sh` of its wrapper.
    let wsps: Vec<u32> = table.iter().filter(|(_, _, e)| base_of(e) == "wsp" || shell_named(e)).map(|r| r.0).collect();
    if !wsps.is_empty() {
        for (pid, command) in ask(&wsps) {
            out.extend(checked_rows(&command).into_iter().map(|row| (pid, row)));
            out.extend(job_row(&command).map(|row| (pid, row)));
        }
    }
    out
}

/// The row of the job whose leader `command` is ([`wsp_host::jobs::wrapped_key`]):
/// its file key's first part, its `%XX` bytes read back
/// ([`wsp_host::jobs::file_key`]).
fn job_row(command: &str) -> Option<String> {
    let key = wsp_host::jobs::wrapped_key(command)?;
    let row = unescape(key.split('+').next()?);
    (!row.is_empty()).then_some(row)
}

/// The running jobs of this machine's manager, for [`runners`]: none where
/// there is no manager, it did not answer, or this process runs its jobs
/// itself (`WSP_JOBS=inproc`); then a job's own leader ([`job_row`]) and a
/// check's own `wsp` still spare what it runs.
fn manager_jobs() -> Vec<wsp_host_api::JobRow> {
    let state = std::env::var_os("WSP_STATE").map(PathBuf::from).unwrap_or_else(Store::default_state);
    match crate::throttle::manager_jobs(&state) {
        Ok(Some(host)) => host.jobs().unwrap_or_default(),
        _ => Vec::new(),
    }
}

/// A part of a job's file name with its `%XX` bytes read back.
fn unescape(part: &str) -> String {
    let b = part.as_bytes();
    let mut out = Vec::with_capacity(b.len());
    let mut i = 0;
    while i < b.len() {
        let hex = (b[i] == b'%').then(|| part.get(i + 1..i + 3)).flatten().and_then(|h| u8::from_str_radix(h, 16).ok());
        match hex {
            Some(byte) => {
                out.push(byte);
                i += 3;
            }
            None => {
                out.push(b[i]);
                i += 1;
            }
        }
    }
    String::from_utf8_lossy(&out).into_owned()
}

/// The nearest of `runners` at or above `at`, climbing until a host that is
/// not one, or the top.
fn runner_above(mut at: u32, runners: &[u32], table: &[(u32, u32, String)]) -> Option<u32> {
    for _ in 0..table.len() {
        if at <= 1 {
            return None;
        }
        if runners.contains(&at) {
            return Some(at);
        }
        let &(_, up, ref exe) = table.iter().find(|(q, _, _)| *q == at)?;
        if host_named(exe).is_some() {
            return None;
        }
        at = up;
    }
    None
}

/// Is `pid`, standing in `tree` by working directory, the tree's to stop?
///
/// **Only what the tree started**: a process whose executable is under the tree
/// or a named dev tool ([`DEV_TOOLS`]), or one that descends from a process that
/// stood in the tree. Never a host, a login shell, or what one of those
/// started, and never an installed server ([`BUILT_SERVERS`]). A process there
/// by working directory alone — somebody started it from that directory — is
/// spared and says why.
///
/// `in_tree` is every pid standing in the tree; `table` is [`exes`]. A seat's
/// processes never get here: `keep` took them before.
pub fn verdict(pid: u32, tree: &Path, in_tree: &[u32], table: &[(u32, u32, String)]) -> Verdict {
    judge(pid, tree, in_tree, table, false)
}

/// The executables that adopt an orphan: pid 1 on a Mac (`launchd`), and on
/// Linux the nearest child subreaper, most often the person's `systemd --user`,
/// whose pid is not 1.
const REAPERS: &[&str] = &["launchd", "systemd", "init"];

/// Is a process whose parent is `ppid` an orphan: its parent pid 1, or a
/// [`REAPERS`] name that adopted it once its starter was gone.
pub fn orphaned(ppid: u32, table: &[(u32, u32, String)]) -> bool {
    ppid <= 1 || table.iter().find(|(p, _, _)| *p == ppid).is_some_and(|(_, _, exe)| REAPERS.contains(&base_of(exe)))
}

/// [`verdict`], and where `orphans` is set an orphan ([`orphaned`]: its starter
/// gone, adopted by pid 1 or a subreaper) that no rule above spares is the
/// tree's to stop (`wsp-691`: eight `yes` burners stood in a member's tree at
/// review, orphaned to launchd). Set
/// for the trees a seat has just left or is waiting in at review, and at a
/// submit: there the tree is the row's alone, so what nobody holds is its
/// leftovers. Hosts, servers, apps and a wsp's check are spared as ever.
pub fn judge(pid: u32, tree: &Path, in_tree: &[u32], table: &[(u32, u32, String)], orphans: bool) -> Verdict {
    judge_among(pid, tree, in_tree, &[], table, orphans)
}

/// [`judge`], sparing what descends from one of `runners`, the pids
/// [`runners`] gives for this tree's row: a running check's, wherever its
/// `wsp` stands (`wsp-426`).
pub fn judge_among(
    pid: u32,
    tree: &Path,
    in_tree: &[u32],
    runners: &[u32],
    table: &[(u32, u32, String)],
    orphans: bool,
) -> Verdict {
    let find = |p: u32| table.iter().find(|(q, _, _)| *q == p);
    let Some(&(_, ppid, ref exe)) = find(pid) else { return Verdict::Spare("it is not in the process table".into()) };
    if let Some(why) = host_named(exe) {
        return Verdict::Spare(format!("{why} that stands there by working directory"));
    }
    // Whatever descends from a `wsp` is that wsp's check (`wsp-541`, `wsp-653`):
    // stopping it sends the row back with exit 137. Asked before the executable
    // and the tool names, so a node under a node, or an esbuild under the tree's
    // `node_modules`, is spared as the cargo is. Only a `wsp` is asked, the nearest
    // host above; a pane's backend does not spare what it holds.
    if let Some(wsp) = wsp_above(ppid, in_tree, table) {
        return Verdict::Spare(format!("it descends from a running wsp, pid {wsp}: a check runs in this tree"));
    }
    // A check run from outside the tree (`wsp-426`): the verifier's `wsp check`,
    // or the manager's job, whose cargo stands in the tree and its wsp does not.
    if let Some(run) = runner_above(pid, runners, table) {
        return Verdict::Spare(format!("it is this row's running check's, pid {run}, run from outside this tree"));
    }
    let built = BUILT_SERVERS.contains(&base_of(exe));
    if exe_in(exe, tree) {
        return Verdict::Stop;
    }
    if built {
        return Verdict::Spare(format!("a server ({}) that is not the tree's build", base_of(exe)));
    }
    // A person's app and all it started, a dev tool by name too: not the row's
    // (`wsp-633`: web-chrome.app's children were TERMed every cycle for a day).
    if let Some(why) = app_named(exe) {
        return Verdict::Spare(format!("{why} that a person launched, standing there by working directory"));
    }
    if let Some((app, why)) = app_above(pid, table) {
        return Verdict::Spare(format!("it descends from {why}, pid {app}, a person's launched app"));
    }
    if DEV_TOOLS.contains(&base_of(exe)) {
        return Verdict::Stop;
    }
    if orphans && orphaned(ppid, table) {
        return Verdict::Stop;
    }
    // Up through its parents, to the first that decides it: a host makes it
    // the host's; one that stood in the tree makes it the tree's. Further up is
    // not asked: every pane has a backend above it.
    let mut at = ppid;
    for _ in 0..table.len() {
        if at <= 1 {
            break;
        }
        let Some(&(_, up, ref up_exe)) = find(at) else { break };
        if let Some(why) = host_named(up_exe) {
            return Verdict::Spare(format!("it descends from {why}"));
        }
        if in_tree.contains(&at) {
            return Verdict::Stop;
        }
        at = up;
    }
    Verdict::Spare("it stands there by working directory alone: nothing in the tree started it".into())
}

/// `lefts` with each tree's pids sorted by [`verdict`] into what is stopped and
/// what is spared. A tree with neither is dropped.
pub fn sorted(lsof: &str, lefts: Vec<Left>, table: &[(u32, u32, String)]) -> Vec<Left> {
    sorted_with(lsof, lefts, table, &|_| false)
}

/// [`sorted`], judging the trees of the rows `orphans` names with
/// [`judge`]'s `orphans` set.
pub fn sorted_with(lsof: &str, lefts: Vec<Left>, table: &[(u32, u32, String)], orphans: &dyn Fn(&str) -> bool) -> Vec<Left> {
    // The check runners are asked once, and only where there is a tree to sort.
    let mut running: Option<Vec<(u32, String)>> = None;
    lefts
        .into_iter()
        .filter_map(|mut l| {
            let in_tree = pids_in(lsof, Path::new(&l.tree));
            let (mut stop, mut spare) = (Vec::new(), Vec::new());
            let orphans = orphans(&l.row);
            let all = running.get_or_insert_with(|| runners(table, &commands, &manager_jobs()));
            // Only a check on this row spares what stands in its tree: a check
            // elsewhere (a seat's suite run, whose tests start processes in
            // trees of their own) does not.
            let ours: Vec<u32> = all.iter().filter(|(_, row)| *row == l.row).map(|r| r.0).collect();
            for p in std::mem::take(&mut l.pids) {
                match judge_among(p, Path::new(&l.tree), &in_tree, &ours, table, orphans) {
                    Verdict::Stop => stop.push(p),
                    Verdict::Spare(why) => spare.push((p, why)),
                }
            }
            l.pids = stop;
            l.spared = spare;
            (!l.pids.is_empty() || !l.spared.is_empty()).then_some(l)
        })
        .collect()
}

/// What a spared process is said once under: its pid, or, where it is spared
/// for descending from an app, that app (`wsp-633`: a new child each cycle is
/// the same lineage and is not named again).
pub fn spared_key(l: &Left, pid: u32, why: &str) -> String {
    if why.starts_with("it descends from an app") {
        format!("{}|{why}", l.tree)
    } else {
        format!("{}|{pid}", l.tree)
    }
}

/// The `cycle.log` line for a process the sweep spared, said once per pid.
pub fn spared_line(l: &Left, pid: u32, why: &str) -> String {
    format!("spared {pid} in finished row {} tree {}: {why}", l.row, util::contract(Path::new(&l.tree)))
}

/// The tree a working directory is in, as `(its name, its root)`: the
/// directory under a `.worktrees/` the path goes through.
fn tree_named(cwd: &str) -> Option<(String, String)> {
    let mut root = PathBuf::new();
    let mut parts = Path::new(cwd).components();
    while let Some(c) = parts.next() {
        root.push(c);
        if c.as_os_str() == WORKTREES.trim_matches('/') {
            let name = parts.next()?;
            root.push(name);
            return Some((name.as_os_str().to_string_lossy().into_owned(), root.display().to_string()));
        }
    }
    None
}

/// The processes in `lsof -d cwd -Fpn` output that stand in the tree of a row
/// `finished` says is finished, less `keep`: grouped by tree, in the order the
/// trees first appear.
///
/// **By the one fact [`pids_in`] uses, and by no name or pattern**: a process
/// is a row's to stop because its working directory is in that row's tree, the
/// row is finished, and nobody holds the row ([`strays`] is why `keep` is every
/// live seat's processes).
pub fn finished_trees(lsof: &str, finished: &dyn Fn(&str) -> bool, keep: &[u32]) -> Vec<Left> {
    let mut out: Vec<Left> = Vec::new();
    let mut pid = None;
    for line in lsof.lines() {
        match line.split_at_checked(1) {
            Some(("p", rest)) => pid = rest.trim().parse::<u32>().ok(),
            Some(("n", rest)) => {
                let Some(p) = pid.filter(|p| !keep.contains(p)) else { continue };
                let Some((name, root)) = tree_named(rest) else { continue };
                if !finished(&name) {
                    continue;
                }
                match out.iter_mut().find(|l| l.tree == root) {
                    Some(l) if !l.pids.contains(&p) => l.pids.push(p),
                    Some(_) => {}
                    None => out.push(Left { row: name, tree: root, pids: vec![p], spared: Vec::new() }),
                }
            }
            _ => {}
        }
    }
    out
}

/// What the finished rows of `store` have left running: a row at `review` or
/// `done`, not run as legs, with no seat open for it and no claim, and a
/// process standing in its tree that is no live seat's and that the tree
/// started ([`verdict`], `wsp-478`).
///
/// **And two more (`wsp-691`)**, judged with [`judge`]'s `orphans` set: a row
/// at `review` whose seat is still open (a member waits there for its
/// verifier, and wsp-693's eight `yes` burners stood beside it for 19
/// minutes), and a row in `ended`, whose seat ended on this tick, whatever its
/// status. Of the seat `own` names for a row at review, only its agent and what
/// the agent started directly ([`agent_of`]) are kept, and only in that row's
/// tree; every other live seat's processes (a verifier working in the tree)
/// are kept whole.
///
/// `None` when no backend answered: a census that failed says nothing about
/// which rows have a seat, so the caller signals and names nothing.
pub fn left_in_finished(
    store: &Store,
    census: Vec<(String, String)>,
    pids: &dyn Fn(&str) -> Option<Vec<u32>>,
    staged: &dyn Fn(&crate::model::Task) -> bool,
    own: &dyn Fn(&str) -> Option<String>,
    ended: &BTreeSet<String>,
) -> Option<Vec<Left>> {
    let mut live = BTreeSet::new();
    let mut keep = crate::base::procs::ancestors(&parents());
    let mut seats: BTreeMap<String, Vec<u32>> = BTreeMap::new();
    for (seat, label) in census {
        live.insert(label.split(" · ").next().unwrap_or_default().trim_start_matches('(').to_string());
        seats.insert(seat.clone(), pids(&seat).unwrap_or_default());
    }
    let at_review = |name: &str| {
        store.task(name).is_some_and(|t| t.status() == crate::model::Status::Review && !staged(&t))
    };
    // The open seat of each row at review: seat to row.
    let owners: BTreeMap<String, String> = live
        .iter()
        .filter(|row| at_review(row))
        .filter_map(|row| own(row).filter(|seat| seats.contains_key(seat)).map(|seat| (seat, row.clone())))
        .collect();
    let mut table = None;
    // Each such row's seat whole: kept in every tree but its own row's.
    let mut theirs: Vec<(String, Vec<u32>)> = Vec::new();
    for (seat, ps) in &seats {
        match owners.get(seat) {
            Some(row) => {
                keep.extend(agent_of(ps, table.get_or_insert_with(exes)));
                theirs.push((row.clone(), ps.clone()));
            }
            None => keep.extend(ps),
        }
    }
    let claims = store.claims();
    // `Some(orphans)` for a row whose tree is swept.
    let class = |name: &str| -> Option<bool> {
        let t = store.task(name)?;
        if ended.contains(name) {
            return Some(true);
        }
        if staged(&t) {
            return None;
        }
        match t.status() {
            crate::model::Status::Review if live.contains(name) => Some(true),
            crate::model::Status::Review | crate::model::Status::Done
                if !live.contains(name) && !claims.contains_key(name) =>
            {
                Some(false)
            }
            _ => None,
        }
    };
    let lsof = LOCAL.cwds(None)?;
    let mut found = finished_trees(&lsof, &|n| class(n).is_some(), &keep);
    for l in &mut found {
        l.pids.retain(|p| !theirs.iter().any(|(row, ps)| row != &l.row && ps.contains(p)));
    }
    found.retain(|l| !l.pids.is_empty());
    // The executables are read only for processes there are to sort (`wsp-480`).
    if found.is_empty() {
        return Some(found);
    }
    let table = table.unwrap_or_else(exes);
    Some(sorted_with(&lsof, found, &table, &|n| class(n) == Some(true)))
}

/// A seat's agent, out of `seat` (every process of the seat) and `table`
/// ([`exes`]): down from the seat's root through the hosts and servers that
/// carry it (`compound-sup`, a login shell), the first process that is
/// neither, and every process it is the direct parent of — its tool shells
/// and its servers (`wsp-691`). What those started is not the agent's to keep.
/// The whole seat when the table could not be read.
pub fn agent_of(seat: &[u32], table: &[(u32, u32, String)]) -> Vec<u32> {
    if table.is_empty() {
        return seat.to_vec();
    }
    let row = |p: u32| table.iter().find(|(q, _, _)| *q == p);
    let children = |of: &[u32]| table.iter().filter(|(_, pp, _)| of.contains(pp)).map(|r| r.0).collect::<Vec<u32>>();
    let carrier = |p: u32| row(p).is_some_and(|(_, _, exe)| host_named(exe).is_some() || BUILT_SERVERS.contains(&base_of(exe)));
    let mut layer: Vec<u32> =
        seat.iter().copied().filter(|p| !row(*p).is_some_and(|(_, pp, _)| seat.contains(pp))).collect();
    let mut keep = Vec::new();
    for _ in 0..=seat.len() {
        if layer.is_empty() {
            break;
        }
        keep.extend(&layer);
        let (carriers, agents): (Vec<u32>, Vec<u32>) = layer.iter().copied().partition(|p| carrier(*p));
        keep.extend(children(&agents));
        layer = children(&carriers).into_iter().filter(|p| !keep.contains(p)).collect();
    }
    keep
}

/// A plain shell, the kind an agent runs a command in the background under.
fn shell_named(exe: &str) -> bool {
    matches!(base_of(exe), "sh" | "bash" | "zsh" | "dash" | "fish")
}

/// What a submit keeps of one live seat (`wsp-691`), `chain` being this
/// process and its parents: another seat's processes whole; of the caller's own
/// seat (one of its processes is on `chain`), its agent and the agent's direct
/// children ([`agent_of`]) less the shells off `chain` — the background shells
/// it left go, and its foreground one is on `chain`.
pub fn kept_at_submit(seat: &[u32], chain: &[u32], table: &[(u32, u32, String)]) -> Vec<u32> {
    if !seat.iter().any(|p| *p > 1 && chain.contains(p)) {
        return seat.to_vec();
    }
    let exe = |p: u32| table.iter().find(|(q, _, _)| *q == p).map(|r| r.2.as_str()).unwrap_or_default();
    agent_of(seat, table).into_iter().filter(|p| chain.contains(p) || !shell_named(exe(*p))).collect()
}

/// What stands in `tree`, row `row`'s, that is the row's to stop: by working
/// directory in the tree and by nothing else, less `keep`, judged with
/// [`judge`]'s `orphans` set (`wsp-691`). Read in `lsof` and `table`.
pub fn left_among(lsof: &str, row: &str, tree: &Path, keep: &[u32], table: &[(u32, u32, String)]) -> Left {
    let tree = util::real(&tree.display().to_string());
    let pids = strays(lsof, &tree, keep);
    let l = Left { row: row.to_string(), tree: tree.display().to_string(), pids, spared: Vec::new() };
    let empty = Left { row: l.row.clone(), tree: l.tree.clone(), pids: Vec::new(), spared: Vec::new() };
    sorted_with(lsof, vec![l], table, &|_| true).pop().unwrap_or(empty)
}

/// [`left_among`] read now, less `keep` and this process's chain.
pub fn left_kept(row: &str, tree: &Path, keep: &[u32]) -> Option<Left> {
    let lsof = LOCAL.cwds(None)?;
    let mut keep = keep.to_vec();
    keep.extend(crate::base::procs::ancestors(&parents()));
    Some(left_among(&lsof, row, tree, &keep, &exes()))
}

/// What row `row` has left standing in `tree` when it is submitted
/// (`submit`, [`kept_at_submit`] for the caller's own seat) or its seat has
/// ended (every live seat kept whole): `wsp-691`. `None` when no backend
/// answered or `lsof` did not run.
pub fn left_in_tree(row: &str, tree: &Path, submit: bool) -> Option<Left> {
    let all = crate::place::local_backends();
    let chain = crate::base::procs::ancestors(&parents());
    let table = exes();
    let mut keep = Vec::new();
    let mut heard = false;
    for backend in here_of(&all) {
        let Ok(census) = backend.census() else { continue };
        heard = true;
        for seat in census.seats() {
            let mut ps = backend.pids(&seat.seat).unwrap_or_default();
            ps.extend(backend.root(&seat.seat));
            keep.extend(if submit { kept_at_submit(&ps, &chain, &table) } else { ps });
        }
    }
    if !heard {
        return None;
    }
    let lsof = LOCAL.cwds(None)?;
    keep.extend(chain);
    Some(left_among(&lsof, row, tree, &keep, &table))
}

/// Each of `pids` with its command line, as `ps` says it now.
pub fn commands(pids: &[u32]) -> Vec<(u32, String)> {
    if pids.is_empty() {
        return Vec::new();
    }
    let out = LOCAL.rows_of(pids, "pid=,command=").unwrap_or_default();
    let mut found: Vec<(u32, String)> = out
        .lines()
        .filter_map(|l| {
            let (pid, rest) = l.trim_start().split_once(char::is_whitespace)?;
            Some((pid.parse().ok()?, rest.trim().to_string()))
        })
        .collect();
    for p in pids {
        if !found.iter().any(|(q, _)| q == p) {
            found.push((*p, "(gone)".into()));
        }
    }
    found.sort_by_key(|(p, _)| pids.iter().position(|q| q == p));
    found
}

/// The resident memory of `pids` in kilobytes.
pub fn rss_kb(pids: &[u32]) -> u64 {
    let Some(out) = LOCAL.rows_of(pids, "rss=") else { return 0 };
    out.lines().filter_map(|l| l.trim().parse::<u64>().ok()).sum()
}

/// The sentence `wsp wip` says of a finished tree with processes in it.
pub fn left_line(l: &Left, kb: u64) -> String {
    let n = l.pids.len();
    format!(
        "{} is finished and has no seat, and {n} process{} ({} MB) still stand in {}: the tick stops them",
        l.row,
        if n == 1 { "" } else { "es" },
        kb / 1024,
        util::contract(Path::new(&l.tree)),
    )
}

/// Take the tree at `dir`, of the repository at `repo`, unless somebody is in
/// it or something is. What its check ran was a job, which the manager ended
/// with it.
fn take(_store: &Store, repo: &Path, dir: &Path, residue: &Residue, standing: &dyn Fn(&Path) -> Option<String>) -> Tree {
    let path = util::contract(dir);
    if let Some(why) = standing(dir) {
        return Tree::Kept { path, why };
    }
    match litter_lately(dir, residue) {
        None => Tree::Kept { path, why: "git could not read it".into() },
        Some(found) if !found.is_empty() => Tree::Kept { path, why: holds_more(&found) },
        Some(_) => {
            Tree::Removed { path, kept: cmd_checkout::remove(repo, dir).kept }
        }
    }
}

/// The repository a project root is in: walked up to the directory holding
/// `.git`, on disk with no git to run.
pub fn repo_of(root: &str) -> Option<PathBuf> {
    let mut dir = util::expand(root);
    while !dir.join(".git").exists() {
        dir = dir.parent()?.to_path_buf();
    }
    dir.join(".git").is_dir().then_some(dir)
}

/// The repositories check trees can be in: every project's roots, each walked
/// up to the directory holding `.git` — on disk, with no git to run, because
/// the tick asks this every twenty seconds and nearly always finds nothing.
/// A root that is itself a linked tree is skipped; its trunk is a root too.
pub fn repos(store: &Store) -> Vec<PathBuf> {
    let mut seen = BTreeSet::new();
    let mut out = Vec::new();
    for dir in store.projects().iter().flat_map(|p| p.roots.iter()).filter_map(|r| repo_of(r)) {
        if seen.insert(util::real(&dir.display().to_string())) {
            out.push(dir);
        }
    }
    out
}

/// `despawn`'s half: the tree of the check named `name`, taken on the rule.
pub fn discard(store: &Store, name: &str, standing: &dyn Fn(&Path) -> Option<String>) -> Tree {
    let Some((list, at)) = named(store, name) else { return Tree::Absent };
    let Some((repo, dir)) = repos(store)
        .into_iter()
        .map(|r| {
            let d = r.join(WORKTREES).join(name);
            (r, d)
        })
        .find(|(_, d)| d.join(".git").exists())
    else {
        return Tree::Absent;
    };
    if in_use(store, &list, at) {
        return Tree::Kept {
            path: util::contract(&dir),
            why: "its check is still open — the tick takes it once the check has settled".into(),
        };
    }
    take(store, &repo, &dir, &Residue::of(store, &repo), standing)
}

/// Every check tree on disk whose check is nobody's, taken on the rule.
/// `standing` is asked only once there is a tree to ask it about.
pub fn sweep(store: &Store, standing: &dyn Fn(&Path) -> Option<String>) -> Vec<Tree> {
    sweep_of(store, None, standing)
}

/// [`sweep`], of the check trees of one list when `only` names it: what a list
/// that has completed takes (`wsp-548`).
pub fn sweep_of(store: &Store, only: Option<&str>, standing: &dyn Fn(&Path) -> Option<String>) -> Vec<Tree> {
    let mut out = Vec::new();
    for repo in repos(store) {
        let Ok(entries) = std::fs::read_dir(repo.join(WORKTREES)) else { continue };
        let residue = Residue::of(store, &repo);
        let mut names: Vec<String> = entries.flatten().map(|e| e.file_name().to_string_lossy().into_owned()).collect();
        names.sort();
        for name in names {
            let dir = repo.join(WORKTREES).join(&name);
            if !dir.join(".git").exists() {
                continue;
            }
            let Some((list, at)) = named(store, &name) else { continue };
            if only.is_some_and(|o| o != list) || in_use(store, &list, at) {
                continue;
            }
            out.push(take(store, &repo, &dir, &residue, standing));
        }
    }
    out
}

/// Who of `census` — `(seat, label, cwd)` — stands in `dir`, the tree of a
/// check that is over: the check's own seats, which nothing else will end
/// (`wsp-089`: one opened with no check to go on stood thirteen hours after
/// its barrier passed, and the sweep kept the tree for it every tick), and the
/// first other seat as the reason to keep the tree. A seat is the check's own
/// by its label, `<list> · barrier <N>`, never by its cwd: other seats stand in
/// a check's tree by cwd too (`wsp-388`). `ended` are seats already ended.
pub fn standing_in(
    census: &[(String, String, String)],
    dir: &Path,
    check: Option<(&str, usize)>,
    ended: &[String],
) -> (Vec<String>, Option<String>) {
    let dir = util::real(&dir.display().to_string());
    let own_label = check.map(|(list, at)| format!("{list} · barrier {at}"));
    let mut own = Vec::new();
    for (seat, label, cwd) in census.iter().filter(|(id, _, _)| !ended.contains(id)) {
        if !util::real(cwd).starts_with(&dir) {
            continue;
        }
        let label = label.trim().trim_start_matches('(').trim_end_matches(')');
        match &own_label {
            Some(l) if label == l || label.strip_prefix(l.as_str()).is_some_and(|r| r.starts_with(" ·")) => own.push(seat.clone()),
            _ => return (own, Some(format!("{seat} is standing in it"))),
        }
    }
    (own, None)
}

/// The kept trees already said in `cycle.log` by this process, with why: a
/// kept tree is found again every tick, and said again only when the reason
/// changes or the daemon starts over.
static SAID: Mutex<BTreeSet<String>> = Mutex::new(BTreeSet::new());

/// [`sweep`], with the live panes as `standing` and what it did in
/// `cycle.log`. `ended` are seats this step has just ended, which a backend
/// can go on listing for a moment after they close.
///
/// Not under test: a fixture's project may be rooted at a real checkout, and
/// the census is a live backend's. The tests drive [`sweep`] with a scratch
/// repository and a `standing` of their own.
pub fn tidy(store: &Store, ended: &[String]) {
    if cfg!(test) {
        return;
    }
    // Asked once, and only once a tree has been found to ask it about.
    let seen = std::cell::OnceCell::new();
    let backend = crate::place_compound::Compound::new();
    let standing = |dir: &Path| -> Option<String> {
        let census = seen.get_or_init(|| {
            backend
                .census()
                .map(|c| c.seats().map(|s| (s.seat.as_str().to_string(), s.label.clone(), s.cwd.clone())).collect::<Vec<_>>())
                .map_err(|e| format!("compound did not say who is standing in it: {e}"))
        });
        let rows = match census {
            Ok(rows) => rows,
            Err(why) => return Some(why.clone()),
        };
        // The sweep asks only of a tree whose check is over.
        let check = dir.file_name().and_then(|n| named(store, &n.to_string_lossy()));
        let (own, other) = standing_in(rows, dir, check.as_ref().map(|(l, at)| (l.as_str(), *at)), ended);
        if other.is_some() {
            return other;
        }
        for seat in own {
            match backend.stop(&crate::place::Seat::new(&seat)) {
                Ok(()) => crate::store::log_line(store, &format!("ended {seat}, a barrier check seat left standing in {} after its check was over", util::contract(dir))),
                Err(e) => return Some(format!("{seat}, its check's own seat, is standing in it and would not end: {e}")),
            }
        }
        None
    };
    let found = believing(|| sweep(store, &standing));
    for tree in found {
        match tree {
            Tree::Removed { path, kept } => {
                let branch = kept.map(|b| format!(" (branch {b} kept — it has commits the trunk has not)")).unwrap_or_default();
                crate::store::log_line(store, &format!("removed barrier check tree {path}: its check is over{branch}"));
            }
            Tree::Kept { path, why } => {
                let line = format!("kept barrier check tree {path}: {why}");
                if SAID.lock().map(|mut s| s.insert(line.clone())).unwrap_or(true) {
                    crate::store::log_line(store, &line);
                }
            }
            Tree::Absent => {}
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// `wsp-665`: a tree found to hold more than build output is not asked
    /// about again for ten minutes, and one found clean is asked every time.
    #[test]
    fn a_kept_trees_litter_is_believed_for_ten_minutes_and_a_clean_one_not_at_all() {
        let dir = PathBuf::from("/nowhere/wsp-665-litter");
        let t0 = Instant::now();
        let asked = std::cell::Cell::new(0);
        let ask = |found: Vec<String>| {
            let asked = &asked;
            move || {
                asked.set(asked.get() + 1);
                Some(found)
            }
        };
        let junk = || vec!["notes.txt".to_string()];
        assert_eq!(litter_at(&dir, t0, ask(junk())), Some(junk()));
        assert_eq!(litter_at(&dir, t0 + Duration::from_secs(300), ask(vec![])), Some(junk()), "believed");
        assert_eq!(asked.get(), 1, "and not asked");
        assert_eq!(litter_at(&dir, t0 + Duration::from_secs(601), ask(vec![])), Some(vec![]), "past ten minutes it is asked");
        assert_eq!(asked.get(), 2);
        assert_eq!(litter_at(&dir, t0 + Duration::from_secs(602), ask(vec![])), Some(vec![]), "a clean tree is never remembered");
        assert_eq!(asked.get(), 3);
        assert_eq!(litter_at(&dir, t0 + Duration::from_secs(603), || None), None, "git unable to read it is not remembered either");
    }
    use crate::model::{Group, Project, Worklist};
    use wsp_base::calls::Command;

    fn run(dir: &Path, args: &[&str]) {
        let out = Command::new("git")
            .arg("-C")
            .arg(dir)
            .args(args)
            .env_remove("GIT_INDEX_FILE")
            .env("GIT_AUTHOR_NAME", "t")
            .env("GIT_AUTHOR_EMAIL", "t@t")
            .env("GIT_COMMITTER_NAME", "t")
            .env("GIT_COMMITTER_EMAIL", "t@t")
            .output()
            .unwrap();
        assert!(out.status.success(), "git {args:?}: {}", String::from_utf8_lossy(&out.stderr));
    }

    /// A scratch repository under the test's own directory — never the tree
    /// the runner is standing in — with a project rooted at it and a list
    /// `run` whose group 1 has the given checks.
    fn scratch(name: &str, checks: &str) -> (util::Isolated, Store, PathBuf) {
        let env = util::isolated(&format!("check-tree-{name}"));
        let repo = env.path("repo");
        std::fs::create_dir_all(&repo).unwrap();
        wsp_testkit::repo(&repo);
        std::fs::write(repo.join(".gitignore"), "target/\n.worktrees/\n").unwrap();
        run(&repo, &["add", ".gitignore"]);
        run(&repo, &["commit", "--quiet", "-m", "first"]);
        let store = Store::open();
        let mut p = Project::new("p");
        p.roots = vec![repo.display().to_string()];
        store.save_project(&p).unwrap();
        let mut w = Worklist::new("run", "p");
        let mut g = Group::default();
        g.members = vec!["p-001".into()];
        crate::checks::push(&mut g.checks, checks);
        w.set_groups(&[g]);
        store.save_worklist(&w).unwrap();
        (env, store, repo)
    }

    fn tree(repo: &Path, name: &str) -> PathBuf {
        let dir = repo.join(WORKTREES).join(name);
        run(repo, &["worktree", "add", "--quiet", "-b", name, &dir.display().to_string(), "master"]);
        dir
    }

    fn nobody(_: &Path) -> Option<String> {
        None
    }

    const ENDED: &str = "2026-10-17T10:00:00Z passes pane:cpd-1 ended";

    /// Today's 56: a settled check whose seat is gone, its tree clean but for
    /// a `target/`. The tick takes it, its branch with it, and a second tick
    /// finds nothing — keyed on the disk, not on a record of having done it.
    #[test]
    fn a_settled_checks_tree_is_taken_and_a_second_sweep_finds_nothing() {
        let (_env, store, repo) = scratch("taken", ENDED);
        let dir = tree(&repo, "run-check-1");
        std::fs::create_dir_all(dir.join("target/debug")).unwrap();
        std::fs::write(dir.join("target/debug/big"), "build output\n").unwrap();

        let out = sweep(&store, &nobody);
        assert!(matches!(out.as_slice(), [Tree::Removed { kept: None, .. }]), "{}", out.len());
        assert!(!dir.exists(), "the directory is still there");
        let list = git(&repo, &["worktree", "list", "--porcelain"]).unwrap();
        assert!(!list.contains("run-check-1"), "git still lists it: {list}");
        assert!(git(&repo, &["rev-parse", "--verify", "--quiet", "refs/heads/run-check-1"]).is_none(), "its branch stayed");

        assert!(sweep(&store, &nobody).is_empty(), "the second sweep found something to do");
    }

    /// Anything beyond `target/` keeps it — a tracked change, a stray file, an
    /// ignored one — and the reason names what is there.
    #[test]
    fn a_tree_holding_anything_beyond_target_is_kept_and_says_what() {
        let (_env, store, repo) = scratch("kept", ENDED);
        let dir = tree(&repo, "run-check-1");
        std::fs::write(dir.join("notes.txt"), "a check wrote this\n").unwrap();
        match sweep(&store, &nobody).as_slice() {
            [Tree::Kept { path, why }] => {
                assert!(why.contains("notes.txt"), "{why}");
                assert!(path.ends_with("run-check-1"), "{path}");
            }
            _ => panic!("a tree with a file in it was not kept"),
        }
        assert!(dir.exists());

        std::fs::remove_file(dir.join("notes.txt")).unwrap();
        std::fs::write(dir.join(".gitignore"), "target/\n.worktrees/\nsecret\n").unwrap();
        assert!(matches!(sweep(&store, &nobody).as_slice(), [Tree::Kept { .. }]), "a tracked change was not kept");
        run(&dir, &["checkout", "--", ".gitignore"]);
        std::fs::create_dir_all(dir.join(".worktrees")).unwrap();
        std::fs::write(dir.join(".worktrees/x"), "ignored, not target\n").unwrap();
        assert!(matches!(sweep(&store, &nobody).as_slice(), [Tree::Kept { .. }]), "an ignored file was not kept");
    }

    /// Spike crates leave ignored files of their own: a standalone `Cargo.lock`
    /// and `__pycache__/`. They are build residue, so the tree goes; what git
    /// does not ignore still keeps it, and says what it is.
    #[test]
    fn ignored_residue_does_not_keep_a_tree_but_untracked_files_do() {
        let (_env, store, repo) = scratch("residue", ENDED);
        std::fs::write(repo.join(".gitignore"), "target/\n.worktrees/\nCargo.lock\n__pycache__/\n*.pyc\n").unwrap();
        run(&repo, &["commit", "--quiet", "-am", "ignore what a spike generates"]);
        let dir = tree(&repo, "run-check-1");
        std::fs::create_dir_all(dir.join("spikes/a/target/debug")).unwrap();
        std::fs::write(dir.join("spikes/a/Cargo.lock"), "# generated\n").unwrap();
        std::fs::create_dir_all(dir.join("spikes/b/__pycache__")).unwrap();
        std::fs::write(dir.join("spikes/b/__pycache__/x.pyc"), "x").unwrap();
        std::fs::write(dir.join("loose.pyc"), "x").unwrap();
        // Tracked source beside the residue, so the directories are not wholly ignored.
        std::fs::write(dir.join("spikes/a/main.rs"), "fn main() {}\n").unwrap();
        run(&dir, &["add", "spikes/a/main.rs"]);
        run(&dir, &["commit", "--quiet", "-m", "a spike"]);

        std::fs::write(dir.join("spikes/a/notes.txt"), "untracked, not ignored\n").unwrap();
        match sweep(&store, &nobody).as_slice() {
            [Tree::Kept { why, .. }] => assert!(why.contains("notes.txt") && !why.contains("Cargo.lock") && !why.contains("pycache"), "{why}"),
            _ => panic!("an untracked file did not keep the tree"),
        }
        std::fs::remove_file(dir.join("spikes/a/notes.txt")).unwrap();
        let out = sweep(&store, &nobody);
        assert!(matches!(out.as_slice(), [Tree::Removed { .. }]), "residue kept the tree: {:?}", out.iter().map(|t| match t { Tree::Kept { why, .. } => why.clone(), _ => "x".into() }).collect::<Vec<_>>());
        assert!(!dir.exists());
    }

    /// A root `Cargo.lock` is the workspace's own and keeps the tree; the list
    /// is extended per project under `## Residue`.
    #[test]
    fn the_residue_list_is_extended_by_the_project() {
        let (_env, store, repo) = scratch("extended", ENDED);
        std::fs::write(repo.join(".gitignore"), "target/\n.worktrees/\nCargo.lock\n.cache/\n").unwrap();
        run(&repo, &["commit", "--quiet", "-am", "ignores"]);
        let dir = tree(&repo, "run-check-1");
        std::fs::write(dir.join("Cargo.lock"), "# the workspace's own\n").unwrap();
        match sweep(&store, &nobody).as_slice() {
            [Tree::Kept { why, .. }] => assert!(why.contains("Cargo.lock"), "{why}"),
            _ => panic!("a root Cargo.lock was read as residue"),
        }
        std::fs::remove_file(dir.join("Cargo.lock")).unwrap();

        std::fs::create_dir_all(dir.join(".cache")).unwrap();
        std::fs::write(dir.join(".cache/x"), "x").unwrap();
        assert!(matches!(sweep(&store, &nobody).as_slice(), [Tree::Kept { .. }]), ".cache/ is not on the built-in list");

        let mut p = store.project("p").unwrap();
        p.body = "## Residue\n- `.cache/`\n".into();
        store.save_project(&p).unwrap();
        assert!(matches!(sweep(&store, &nobody).as_slice(), [Tree::Removed { .. }]), "the project's own list was not read");
    }

    /// A live pane standing in it keeps it, and so does a check still
    /// reading, or one whose seat has not been ended — the next check on the
    /// group is started in the same tree.
    #[test]
    fn a_tree_somebody_is_in_or_whose_check_is_still_somebodys_is_left() {
        let (_env, store, repo) = scratch("standing", ENDED);
        let dir = tree(&repo, "run-check-1");
        let here = |d: &Path| (util::real(&d.display().to_string()) == util::real(&dir.display().to_string())).then(|| "cpd-7 is standing in it".to_string());
        match sweep(&store, &here).as_slice() {
            [Tree::Kept { why, .. }] => assert!(why.contains("cpd-7"), "{why}"),
            _ => panic!("a tree a pane is standing in was not kept"),
        }

        for checks in ["2026-10-17T10:00:00Z running pane:cpd-1", "2026-10-17T10:00:00Z running", "2026-10-17T10:00:00Z passes pane:cpd-1"] {
            let mut w = store.worklist("run").unwrap();
            let mut groups = w.groups();
            groups[0].checks.clear();
            crate::checks::push(&mut groups[0].checks, checks);
            w.set_groups(&groups);
            store.save_worklist(&w).unwrap();
            assert!(sweep(&store, &nobody).is_empty(), "a tree was swept while its check was somebody's: {checks}");
            assert!(matches!(discard(&store, "run-check-1", &nobody), Tree::Kept { .. }), "{checks}");
        }
        assert!(dir.exists());
    }

    /// Only a check's: a task's tree, a name that parses but whose list is not
    /// in the store, and a task whose id happens to parse are never swept.
    #[test]
    fn only_a_checks_tree_is_swept() {
        let (_env, store, repo) = scratch("only", ENDED);
        let task = tree(&repo, "p-001");
        let stranger = tree(&repo, "nolist-check-1");
        assert!(sweep(&store, &nobody).is_empty());
        assert!(task.exists() && stranger.exists());

        let mut t = crate::model::Task::new("a task whose id parses", "run-check-2");
        t.project = Some("p".into());
        store.save_task(&t).unwrap();
        let looks = tree(&repo, "run-check-2");
        assert!(sweep(&store, &nobody).is_empty(), "a task's tree was read as a check's");
        assert!(looks.exists());
    }

    /// `despawn`'s half takes it on the same rule, and finds no tree as no tree.
    #[test]
    fn despawn_takes_a_settled_checks_tree_by_name() {
        let (_env, store, repo) = scratch("despawn", ENDED);
        assert!(matches!(discard(&store, "run-check-1", &nobody), Tree::Absent));
        let dir = tree(&repo, "run-check-1");
        assert!(matches!(discard(&store, "run-check-1", &nobody), Tree::Removed { .. }));
        assert!(!dir.exists());
    }

    /// What `lsof -d cwd -Fpn` prints, narrowed to the tree: the processes
    /// whose working directory is in it, below it too, and a sibling whose name
    /// merely begins the same is not.
    #[test]
    fn a_process_is_the_trees_by_its_working_directory() {
        let lsof = "p10\nn/r/.worktrees/web-check-3\np11\nn/r/.worktrees/web-check-3/spikes/web-chrome\np12\nn/r/.worktrees/web-check-30\np13\nn/\np11\nn/r/.worktrees/web-check-3/x\n";
        assert_eq!(pids_in(lsof, Path::new("/r/.worktrees/web-check-3")), vec![10, 11]);
    }

    /// `wsp-388`: a seat's supervisor can stand in a check's tree by cwd
    /// (it took the cwd of the `wsp worklist go` that seated it), and ending the
    /// check signalled it. Of the processes with their cwd in the tree, a
    /// seat's are not the tree's: the dev server is.
    #[test]
    fn a_seat_standing_in_a_checks_tree_by_cwd_is_not_the_trees_to_stop() {
        let lsof = "p10\nn/r/.worktrees/pre-prep-check-4\np11\nn/r/.worktrees/pre-prep-check-4\np12\nn/r/.worktrees/pre-prep-check-4/web\np13\nn/r\n";
        let dir = Path::new("/r/.worktrees/pre-prep-check-4");
        assert_eq!(strays(lsof, dir, &[]), vec![10, 11, 12]);
        // 11 is a member's supervisor, 13 a governor's; 10 and 12 are servers.
        assert_eq!(strays(lsof, dir, &[11, 13]), vec![10, 12]);
    }

    /// `wsp-453`: of what `lsof` says stands in `.worktrees/`, the processes of
    /// finished rows' trees, grouped by tree, less a live seat's.
    #[test]
    fn a_finished_rows_processes_are_named_by_their_tree_and_a_seats_are_spared() {
        let lsof = "p10\nn/r/.worktrees/c-1\np11\nn/r/.worktrees/c-1/web\np12\nn/r/.worktrees/c-2\np13\nn/r/.worktrees/c-3/x\np14\nn/r\np10\nn/r/.worktrees/c-1\n";
        let finished = |n: &str| n == "c-1" || n == "c-3";
        assert_eq!(
            finished_trees(lsof, &finished, &[]),
            vec![
                Left { row: "c-1".into(), tree: "/r/.worktrees/c-1".into(), pids: vec![10, 11], spared: vec![] },
                Left { row: "c-3".into(), tree: "/r/.worktrees/c-3".into(), pids: vec![13], spared: vec![] },
            ]
        );
        let kept = finished_trees(lsof, &finished, &[11, 13]);
        assert_eq!(kept.len(), 1);
        assert_eq!(kept[0].pids, vec![10]);
    }

    fn proc(pid: u32, ppid: u32, exe: &str) -> (u32, u32, String) {
        (pid, ppid, exe.to_string())
    }

    fn judged(pid: u32, in_tree: &[u32], table: &[(u32, u32, String)]) -> Verdict {
        verdict(pid, Path::new("/r/.worktrees/c-1"), in_tree, table)
    }

    /// `wsp-478`: the backend server and a `wsp` stood in a finished tree by
    /// working directory alone and were TERMed.
    #[test]
    fn a_backend_or_wsp_standing_in_a_finished_tree_is_spared() {
        let table = [proc(31702, 1, "/opt/homebrew/bin/tmux"), proc(23862, 1, "/Users/e/.local/bin/wsp")];
        for pid in [31702, 23862] {
            assert!(matches!(judged(pid, &[31702, 23862], &table), Verdict::Spare(_)), "{pid}");
        }
        // Even a `wsp` built in the tree: a daemon may be running from it.
        let built = [proc(5, 1, "/r/.worktrees/c-1/target/debug/wsp")];
        assert!(matches!(judged(5, &[5], &built), Verdict::Spare(_)));
    }

    #[test]
    fn a_compound_sup_is_spared_unless_the_tree_built_it() {
        let installed = [proc(7, 1, "/Users/e/.local/bin/compound-sup")];
        assert!(matches!(judged(7, &[7], &installed), Verdict::Spare(_)));
        let built = [proc(8, 1, "/r/.worktrees/c-1/target/debug/compound-sup")];
        assert_eq!(judged(8, &[8], &built), Verdict::Stop, "a stale build of the row's own");
    }

    #[test]
    fn a_login_shell_and_ssh_are_spared_and_anything_unnamed_standing_there_is() {
        let table = [proc(1000, 1, "-zsh"), proc(1001, 1, "/usr/bin/ssh"), proc(1002, 1, "/usr/local/bin/some-server")];
        for pid in [1000, 1001, 1002] {
            assert!(matches!(judged(pid, &[1000, 1001, 1002], &table), Verdict::Spare(_)), "{pid}");
        }
    }

    #[test]
    fn a_dev_tool_and_what_descends_from_the_tree_are_stopped() {
        let table = [
            proc(10, 1, "/usr/local/bin/node"),
            proc(20, 1, "/bin/sh"),
            proc(21, 20, "/usr/bin/some-watcher"),
            proc(22, 21, "/usr/bin/grandchild"),
        ];
        assert_eq!(judged(10, &[10], &table), Verdict::Stop, "a dev tool by name");
        assert_eq!(judged(21, &[20, 21], &table), Verdict::Stop, "a child of a process in the tree");
        assert_eq!(judged(22, &[20, 22], &table), Verdict::Stop, "and its descendants");
        assert!(matches!(judged(21, &[21], &table), Verdict::Spare(_)), "its parent was not in the tree");
    }

    /// `wsp-691`: an orphan nothing spares is the tree's where the tree is a
    /// seat's that has ended or waits at review; a host, a server or an app
    /// standing there is spared as ever.
    #[test]
    fn an_orphan_is_stopped_where_orphans_are_judged_and_a_host_or_server_never() {
        let table = [
            proc(40, 1, "yes"),
            proc(41, 1, "/usr/local/bin/some-server"),
            proc(42, 1, "/opt/homebrew/bin/tmux"),
            proc(43, 1, "/Users/e/.local/bin/compound-sup"),
            proc(44, 1, "/Users/e/Applications/web-chrome.app/Contents/MacOS/web-chrome"),
        ];
        let in_tree = [40, 41, 42, 43, 44];
        let tree = Path::new("/r/.worktrees/c-1");
        assert!(matches!(judge(40, tree, &in_tree, &table, false), Verdict::Spare(_)), "today's rule, unchanged");
        assert_eq!(judge(40, tree, &in_tree, &table, true), Verdict::Stop);
        // On Linux the orphan's parent is the person's `systemd --user`, a
        // subreaper, not pid 1; a child of a plain shell is no orphan.
        let linux = [proc(1361, 1, "systemd"), proc(45, 1361, "yes"), proc(46, 1, "bash"), proc(47, 46, "yes")];
        assert_eq!(judge(45, tree, &[45], &linux, true), Verdict::Stop, "adopted by systemd --user");
        assert!(matches!(judge(47, tree, &[47], &linux, true), Verdict::Spare(_)), "a shell's child is not an orphan");
        assert_eq!(judge(41, tree, &in_tree, &table, true), Verdict::Stop);
        for pid in [42, 43, 44] {
            assert!(matches!(judge(pid, tree, &in_tree, &table, true), Verdict::Spare(_)), "{pid}");
        }
    }

    /// `wsp-691`: a compound seat's agent is the child of its `compound-sup`;
    /// it and what it started directly are kept, and no further down.
    #[test]
    fn a_seats_agent_is_found_under_its_carrier_and_kept_with_its_direct_children() {
        let table = [
            proc(50, 1, "/Users/e/.local/bin/compound-sup"),
            proc(51, 50, "claude"),
            proc(52, 51, "/bin/zsh"),
            proc(53, 52, "/usr/bin/yes"),
            proc(54, 51, "node"),
        ];
        let mut kept = agent_of(&[50, 51, 52, 53, 54], &table);
        kept.sort();
        assert_eq!(kept, vec![50, 51, 52, 54]);
        assert_eq!(agent_of(&[7, 8], &[]), vec![7, 8], "no table: the seat whole");
    }

    /// `wsp-691`: at submit the caller's own seat keeps its agent, its servers
    /// and the chain the submit runs on; a background shell it left goes, and
    /// another seat is kept whole.
    #[test]
    fn the_submit_keeps_the_callers_chain_and_agent_and_drops_its_background_shells() {
        let table = [
            proc(50, 1, "/Users/e/.local/bin/compound-sup"),
            proc(51, 50, "claude"),
            proc(52, 51, "/bin/zsh"),
            proc(53, 52, "/Users/e/.local/bin/wsp"),
            proc(55, 51, "/bin/bash"),
            proc(56, 55, "yes"),
            proc(57, 51, "node"),
        ];
        let chain = [53, 52, 51, 50, 1];
        let mut kept = kept_at_submit(&[50, 51, 52, 53, 55, 56, 57], &chain, &table);
        kept.sort();
        assert_eq!(kept, vec![50, 51, 52, 57]);
        assert_eq!(kept_at_submit(&[60, 61], &chain, &table), vec![60, 61], "another seat, whole");
        let lsof = "p51\nn/r/.worktrees/c-1\np52\nn/r/.worktrees/c-1\np53\nn/r/.worktrees/c-1\np55\nn/r/.worktrees/c-1\np56\nn/r/.worktrees/c-1/web\np57\nn/r/.worktrees/c-1\np70\nn/r/.worktrees/c-2\n";
        let mut keep = kept;
        keep.extend(chain);
        let left = left_among(lsof, "c-1", Path::new("/r/.worktrees/c-1"), &keep, &table);
        assert_eq!(left.pids, vec![55, 56], "the background shell and its yes; nothing outside the tree");
    }

    /// `wsp-633`: a person's app's children were TERMed by cwd every cycle.
    #[test]
    fn a_persons_app_and_what_it_started_are_spared_even_a_dev_tool() {
        let table = [
            proc(85319, 1, "/Users/e/Applications/web-chrome.app/Contents/MacOS/web-chrome"),
            proc(85320, 85319, "/usr/local/bin/helper"),
            proc(85321, 85320, "/usr/local/bin/node"),
            proc(60, 1, "/opt/homebrew/bin/tmux"),
            proc(61, 60, "/bin/sh"),
            proc(62, 61, "/usr/local/bin/node"),
        ];
        let all = [85319, 85320, 85321, 62];
        for pid in [85319, 85320, 85321] {
            let v = judged(pid, &all, &table);
            assert!(matches!(&v, Verdict::Spare(_)), "{pid}: {v:?}");
        }
        // Under a host the host decides, as before: a dev tool is still the tree's.
        assert_eq!(judged(62, &all, &table), Verdict::Stop);
        let l = Left { row: "c-1".into(), tree: "/r/.worktrees/c-1".into(), pids: vec![], spared: vec![] };
        let (Verdict::Spare(a), Verdict::Spare(b)) = (judged(85320, &all, &table), judged(85321, &all, &table)) else { panic!() };
        assert_eq!(spared_key(&l, 85320, &a), spared_key(&l, 85321, &b), "one lineage, one name");
    }

    /// `wsp-541`: the sweep killed the cargo a `wsp check` ran in a finished row's tree.
    #[test]
    fn a_cargo_that_a_wsp_check_started_in_a_finished_tree_is_spared() {
        let table = [
            proc(60, 1, "/opt/homebrew/bin/tmux"),
            proc(61, 60, "/bin/sh"),
            proc(19582, 61, "/usr/local/bin/wsp"),
            proc(20694, 19582, "/Users/e/.cargo/bin/cargo"),
            proc(20699, 20694, "/Users/e/.rustup/bin/rustc"),
            proc(62, 61, "/usr/local/bin/cargo"),
        ];
        let all = [19582, 20694, 20699, 62];
        for pid in [19582, 20694, 20699] {
            let v = judged(pid, &all, &table);
            assert!(matches!(&v, Verdict::Spare(_)), "{pid}: {v:?}");
        }
        assert_eq!(judged(62, &all, &table), Verdict::Stop, "a cargo only under a pane's backend is still the tree's");
    }

    /// `wsp-653`: the process table at 04:22:45Z, a web-chrome row's `npx vitest`
    /// under `sh -c` under `wsp`: the wsp and its shell were spared, the rest stopped.
    #[test]
    fn everything_a_wsp_check_runs_in_a_finished_tree_is_spared_whatever_runs_it() {
        let table = [
            proc(60, 1, "/opt/homebrew/bin/tmux"),
            proc(61, 60, "/bin/sh"),
            proc(85802, 61, "/usr/local/bin/wsp"),
            proc(85960, 85802, "/bin/sh"),
            proc(85961, 85960, "node"),
            proc(86241, 85961, "node"),
            proc(86475, 86241, "/opt/homebrew/bin/node"),
            proc(90715, 86475, "node_modules/@esbuild/darwin-arm64/bin/esbuild"),
            proc(90733, 86475, "/r/.worktrees/c-1/node_modules/.bin/vitest"),
            proc(90735, 86475, "/usr/bin/some-worker"),
            proc(70, 61, "node"),
            proc(71, 70, "node"),
        ];
        let all = [85802, 85960, 85961, 86241, 86475, 90715, 90733, 90735, 70, 71];
        for pid in [85802, 85960, 85961, 86241, 86475, 90715, 90733, 90735] {
            let v = judged(pid, &all, &table);
            assert!(matches!(&v, Verdict::Spare(_)), "{pid}: {v:?}");
        }
        for pid in [70, 71] {
            assert_eq!(judged(pid, &all, &table), Verdict::Stop, "{pid}: a node only under a pane's backend is still the tree's");
        }
    }

    /// `wsp-426`: wsp-307's verifier ran `wsp check` from outside the row's
    /// tree, and at review the sweep TERMed its cargo and rustc (exit 143). A
    /// check on the tree's row is spared wherever its wsp stands, whether the
    /// wsp runs the job or a manager does (by its job's key); an orphan beside
    /// it, a cargo under a plain wsp elsewhere (`wsp-653`), and what a check
    /// on another row runs, still go.
    #[test]
    fn a_check_on_the_row_run_from_outside_its_tree_is_spared_and_an_orphan_beside_it_is_not() {
        use wsp_host_api::{At, JobClass, JobRow, JobState};
        let tree = Path::new("/r/.worktrees/c-1");
        let table = [
            proc(60, 1, "/opt/homebrew/bin/tmux"),
            proc(61, 60, "/bin/zsh"),
            proc(70, 61, "/usr/local/bin/wsp"),
            proc(71, 70, "/bin/sh"),
            proc(72, 71, "/Users/e/.cargo/bin/cargo"),
            proc(73, 72, "/Users/e/.rustup/bin/rustc"),
            proc(80, 1, "/usr/local/bin/wsp-manager"),
            proc(81, 80, "sh"),
            proc(82, 81, "cargo"),
            proc(83, 80, "sh"),
            proc(84, 83, "cargo"),
            proc(90, 61, "/usr/local/bin/wsp"),
            proc(91, 90, "cargo"),
            proc(95, 1, "yes"),
        ];
        let ask = |pids: &[u32]| -> Vec<(u32, String)> {
            let line = |p: &u32| match p {
                70 => "/usr/local/bin/wsp check --at abc1234 c-1",
                90 => "wsp sandbox exec cargo test",
                71 => "/bin/sh -c cargo test",
                _ => "(gone)",
            };
            pids.iter().map(|p| (*p, line(p).to_string())).collect()
        };
        let job = |row: &str, pgid: u32, state| JobRow {
            key: crate::host::job_key(row, 0, "throttle:test@1.1"),
            class: JobClass::Build,
            pgid,
            started: At(0),
            state,
        };
        let jobs = [job("c-1", 81, JobState::Running), job("c-2", 83, JobState::Running), job("c-1", 99, JobState::Ended)];
        let run = runners(&table, &ask, &jobs);
        assert_eq!(run, vec![(81, "c-1".into()), (83, "c-2".into()), (70, "abc1234".into()), (70, "c-1".into())]);
        let ours: Vec<u32> = run.iter().filter(|(_, r)| r == "c-1").map(|r| r.0).collect();
        let in_tree = [72, 73, 82, 84, 91, 95];
        for pid in [72, 73, 82] {
            let v = judge_among(pid, tree, &in_tree, &ours, &table, true);
            assert!(matches!(&v, Verdict::Spare(why) if why.contains("running check's")), "{pid}: {v:?}");
        }
        for pid in [84, 91, 95] {
            assert_eq!(judge_among(pid, tree, &in_tree, &ours, &table, true), Verdict::Stop, "{pid}");
        }
        assert_eq!(checked_rows("/bin/sh /home/e/bin/wsp check wsp-307"), vec!["wsp-307"], "a wsp script under its shell");
        assert_eq!(checked_rows("wsp --quiet verified --holds wsp-307"), vec!["wsp-307"]);
        assert!(checked_rows("/bin/sh -c wsp check wsp-307").is_empty());
        assert!(checked_rows("wsp land wsp-307").is_empty());
        assert!(checked_rows("wspx check a").is_empty());
    }

    /// `wsp-444`: a job's leader names its row on its own command line, so
    /// what a row's check step runs in its tree is spared whoever runs the
    /// step and with no job table to ask: a manager (compound-react-058's
    /// `npm run check`, under `wsp-manager`, for the `wsp check` its `wsp
    /// submit` started), the daemon in its own process (`WSP_JOBS=inproc`), a
    /// `wsp review` or `wsp submit` at its ready step, and a `wsp` whose verb
    /// is no check verb (the run's barrier step under `wsp worklist advance`),
    /// each from outside the tree. A job of another row, a plain `sh -c` in the
    /// tree, and an orphan still go.
    #[test]
    fn a_rows_check_step_is_spared_by_its_jobs_leader_whatever_runs_it() {
        let tree = Path::new("/r/.worktrees/c-1");
        let wrapper = |row: &str, n: u32| {
            format!(
                "sh -c e=$1; shift \"$@\" s=$? {{ echo \"exit $s\"; times; }} > \"$e.tmp\" 2>/dev/null && mv -f \"$e.tmp\" \"$e\" kill -KILL 0 sh /x/state/jobs/{row}+0+check%3Aweb-chrome_check@77.1.{n}.exit env -u WSP_EXIT -- sh -c cd spikes/web-chrome && npm run check"
            )
        };
        // (the step's runner, its exe, the leader's pid): each leader has an
        // `npm run check` under it, and a `node scripts/check.mjs` under that.
        let runs: [(u32, &str); 5] = [
            (100, "/home/e/.local/bin/wsp-manager"),
            (110, "/home/e/.local/bin/wsp"),
            (120, "/home/e/.local/bin/wsp"),
            (130, "/home/e/.local/bin/wsp"),
            (140, "/home/e/.local/bin/wsp"),
        ];
        let mut table = vec![proc(1361, 1, "systemd")];
        for (run, exe) in runs {
            table.extend([proc(run, 1361, exe), proc(run + 1, run, "sh"), proc(run + 2, run + 1, "npm"), proc(run + 3, run + 2, "node")]);
        }
        table.extend([
            // Another row's job in this tree, a plain shell's npm, an orphan.
            proc(150, 1361, "/home/e/.local/bin/wsp-manager"),
            proc(151, 150, "sh"),
            proc(152, 151, "npm"),
            proc(160, 1361, "/home/e/.local/bin/wsp-manager"),
            proc(161, 160, "sh"),
            proc(162, 161, "npm"),
            proc(170, 1361, "yes"),
        ]);
        let ask = move |pids: &[u32]| -> Vec<(u32, String)> {
            let line = |p: u32| match p {
                100 => "/home/e/.local/bin/wsp-manager --exec-dir /x/state --port 7487".to_string(),
                110 => "/home/e/.local/bin/wsp daemon".to_string(),
                120 => "/home/e/.local/bin/wsp review c-1 -".to_string(),
                130 => "/home/e/.local/bin/wsp submit c-1".to_string(),
                140 => "/home/e/.local/bin/wsp worklist advance verify-fix-2".to_string(),
                101 | 111 | 121 | 131 | 141 => wrapper("c-1", p),
                151 => wrapper("c-2", p),
                161 => "sh -c npm run dev".to_string(),
                _ => "(gone)".to_string(),
            };
            pids.iter().map(|p| (*p, line(*p))).collect()
        };
        let run = runners(&table, &ask, &[]);
        let ours: Vec<u32> = run.iter().filter(|(_, r)| r == "c-1").map(|r| r.0).collect();
        assert_eq!(ours, vec![101, 111, 121, 131, 141], "{run:?}");
        assert!(run.contains(&(151, "c-2".into())), "{run:?}");
        let in_tree: Vec<u32> = table.iter().map(|r| r.0).filter(|p| ![1361, 100, 110, 120, 130, 140, 150, 160].contains(p)).collect();
        for leader in [101, 111, 121, 131, 141] {
            for pid in [leader, leader + 1, leader + 2] {
                let v = judge_among(pid, tree, &in_tree, &ours, &table, true);
                assert!(matches!(&v, Verdict::Spare(why) if why.contains("running check's")), "{pid}: {v:?}");
            }
        }
        for pid in [152, 162, 170] {
            assert_eq!(judge_among(pid, tree, &in_tree, &ours, &table, true), Verdict::Stop, "{pid}");
        }
        assert_eq!(job_row(&wrapper("c%2B1", 1)).as_deref(), Some("c+1"), "the key's row, unescaped");
        assert_eq!(job_row("sh -c cd spikes/web-chrome && npm run check"), None);
    }

    #[test]
    fn what_a_host_started_is_the_hosts() {
        let table = [proc(30, 1, "/opt/homebrew/bin/tmux"), proc(31, 30, "/usr/bin/some-pane")];
        assert!(matches!(judged(31, &[30, 31], &table), Verdict::Spare(_)));
    }

    #[test]
    fn sorting_a_tree_splits_what_it_started_from_what_only_stands_there() {
        let lsof = "p31702\nn/r/.worktrees/c-1\np40\nn/r/.worktrees/c-1/web\n";
        let table = [proc(31702, 1, "/opt/homebrew/bin/tmux"), proc(40, 1, "/usr/local/bin/vite")];
        let lefts = vec![Left { row: "c-1".into(), tree: "/r/.worktrees/c-1".into(), pids: vec![31702, 40], spared: vec![] }];
        let out = sorted(lsof, lefts, &table);
        assert_eq!(out[0].pids, vec![40]);
        assert_eq!(out[0].spared.len(), 1);
        assert_eq!(out[0].spared[0].0, 31702);
        assert!(spared_line(&out[0], 31702, &out[0].spared[0].1).starts_with("spared 31702 in finished row c-1"));
    }

    /// `wsp-089`. In a tree whose check is over, the check's own seat — by its
    /// label — is named for ending and keeps nothing; another seat standing
    /// there by cwd keeps the tree, as it always did.
    #[test]
    fn a_passed_checks_own_seat_is_ended_and_another_keeps_the_tree() {
        let dir = Path::new("/nowhere/r/.worktrees/run-check-1");
        let at = |s: &str, l: &str, c: &str| (s.to_string(), l.to_string(), c.to_string());
        let own = [at("w9:p2", "run · barrier 1", "/nowhere/r/.worktrees/run-check-1"), at("w1:p1", "t-9", "/elsewhere")];
        assert_eq!(standing_in(&own, dir, Some(("run", 1)), &[]), (vec!["w9:p2".to_string()], None));
        let wrapped = [at("w9:p2", "(run · barrier 1 · opus)", "/nowhere/r/.worktrees/run-check-1/sub")];
        assert_eq!(standing_in(&wrapped, dir, Some(("run", 1)), &[]).0, vec!["w9:p2".to_string()]);
        let tenth = [at("w9:p3", "run · barrier 10", "/nowhere/r/.worktrees/run-check-1")];
        assert_eq!(standing_in(&tenth, dir, Some(("run", 1)), &[]), (vec![], Some("w9:p3 is standing in it".to_string())), "barrier 10 is not 1's");
        let member = [at("w2:p1", "t-1", "/nowhere/r/.worktrees/run-check-1")];
        assert_eq!(standing_in(&member, dir, Some(("run", 1)), &[]).1.as_deref(), Some("w2:p1 is standing in it"));
        assert_eq!(standing_in(&member, dir, Some(("run", 1)), &["w2:p1".to_string()]), (vec![], None), "an ended seat is nobody");
        assert_eq!(standing_in(&own, dir, None, &[]).1.as_deref(), Some("w9:p2 is standing in it"), "no check named: nothing is ended");
    }

    /// The trees of 2026-11-05: `npm ci` and a dev server left `node_modules/`,
    /// `dist/`, `test-results/` and the server's own files, all ignored. They
    /// are build output; a changed or untracked file beside them is not.
    #[test]
    fn a_web_checks_node_modules_and_dev_server_files_are_residue() {
        let (_env, store, repo) = scratch("web", ENDED);
        std::fs::write(repo.join(".gitignore"), "target/\n.worktrees/\nnode_modules/\ndist/\ntest-results/\n.dev-port\n.dev.log\n.watch/\n").unwrap();
        run(&repo, &["add", ".gitignore"]);
        run(&repo, &["commit", "--quiet", "-m", "ignore"]);
        let dir = tree(&repo, "run-check-1");
        for f in ["node_modules/a/x.js", "dist/i.js", "test-results/r.json", ".dev-port", ".dev.log", ".watch/w"] {
            let p = dir.join(f);
            std::fs::create_dir_all(p.parent().unwrap()).unwrap();
            std::fs::write(p, "x").unwrap();
        }
        let found = sweep(&store, &nobody);
        assert!(matches!(found.as_slice(), [Tree::Removed { .. }]), "the web residue kept the tree: {}", found.len());
        assert!(!dir.exists());
    }
}
