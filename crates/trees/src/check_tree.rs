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

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};
use std::sync::Mutex;

use crate::cmd_checkout::{self, Tree, WORKTREES};
use crate::cmd_verify::git;
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

/// Why a tree is kept for what [`litter`] found in it.
pub fn holds_more(found: &[String]) -> String {
    let shown = found.iter().take(3).cloned().collect::<Vec<_>>().join(", ");
    let more = if found.len() > 3 { format!(" and {} more", found.len() - 3) } else { String::new() };
    format!("it holds more than build output: {shown}{more}")
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

/// Every process of every seat that is open now on `backends`: the roots and
/// everything under them. A seat's processes are ended by its own close and by
/// nothing else.
///
/// `None` when no backend answered: a census that failed says nothing about
/// whose processes are whose, so the caller signals nothing.
fn seated(backends: &[&dyn Place]) -> Option<Vec<u32>> {
    let mut out = Vec::new();
    let mut heard = false;
    for backend in backends {
        let Ok(census) = backend.census() else { continue };
        heard = true;
        for seat in census.seats() {
            out.extend(backend.pids(&seat.seat).unwrap_or_default());
            out.extend(backend.root(&seat.seat));
        }
    }
    heard.then_some(out)
}

/// What is running with `dir` as its working directory, other than this process
/// and its parents, and other than a seat's of `backends`.
fn running_among(dir: &Path, backends: &[&dyn Place]) -> Vec<u32> {
    let Ok(out) = wsp_base::calls::Command::new("lsof").args(["-d", "cwd", "-Fpn"]).output() else { return Vec::new() };
    let Some(seats) = seated(backends) else { return Vec::new() };
    let dir = util::real(&dir.display().to_string());
    let mut keep = crate::base::procs::ancestors(&crate::base::procs::processes());
    keep.extend(seats);
    strays(&String::from_utf8_lossy(&out.stdout), &dir, &keep)
}

/// [`running_among`] the backends wsp can spawn onto here.
pub fn running_in(dir: &Path) -> Vec<u32> {
    let all = crate::place::local_backends();
    running_among(dir, &here_of(&all))
}

/// The backends of `all` that answer on this machine now.
fn here_of(all: &[Box<dyn Place>; 2]) -> Vec<&dyn Place> {
    let herdr_here = crate::herdr::available_now();
    all.iter()
        .zip(crate::place::LOCAL_BACKEND_NAMES)
        .filter(|(_, name)| *name != "herdr" || herdr_here)
        .map(|(b, _)| b.as_ref())
        .collect()
}

/// Every process of every live seat here, and this process's ancestors: what
/// `wsp reap` never stops. `None` when no backend answered — a census that
/// failed says nothing about whose processes are whose.
pub fn live_seat_pids() -> Option<Vec<u32>> {
    let all = crate::place::local_backends();
    let mut keep = crate::base::procs::ancestors(&crate::base::procs::processes());
    keep.extend(seated(&here_of(&all))?);
    Some(keep)
}

/// Send `sig` to `pids`, and say nothing of it: a process that has gone is the
/// outcome wanted.
pub fn signal(sig: &str, pids: &[u32]) {
    if !pids.is_empty() {
        let _ = wsp_base::calls::Command::new("kill")
            .arg(sig)
            .args(pids.iter().map(|p| p.to_string()))
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .status();
    }
}

/// Stop the dev servers and watchers a check left running in its tree (a Vite
/// server, `tsgo` and Vitest watchers were still up 21 hours after their
/// check, `wsp-373`): `TERM`, a moment, then `KILL` for what is left. A process
/// whose working directory is in the tree is the tree's unless it is a seat's
/// ([`strays`]). Returns the pids sent `TERM` and the pids sent `KILL`. Not
/// under test: it would signal the machine's own processes; [`strays`] is what
/// the tests drive.
fn stop_running(dir: &Path) -> (Vec<u32>, Vec<u32>) {
    if cfg!(test) {
        return (Vec::new(), Vec::new());
    }
    let found = running_in(dir);
    signal("-TERM", &found);
    let mut killed = Vec::new();
    if !found.is_empty() {
        std::thread::sleep(std::time::Duration::from_millis(1500));
        killed = running_in(dir);
        signal("-KILL", &killed);
    }
    (found, killed)
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
/// stop, whatever its working directory (`wsp-478`: the herdr server and a
/// `wsp` were TERMed for standing in a finished tree they were started from).
const HOSTS: &[&str] = &["herdr", "wsp", "ssh", "sshd", "ssh-agent", "mosh-server", "tmux", "screen"];

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
    let Ok(out) = wsp_base::calls::Command::new("ps").args(["-A", "-o", "pid=,ppid=,comm="]).output() else { return Vec::new() };
    String::from_utf8_lossy(&out.stdout)
        .lines()
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

/// Does the executable live under `tree`? A relative one was run from the
/// working directory, which is in the tree.
fn exe_in(exe: &str, tree: &Path) -> bool {
    let p = Path::new(exe);
    (!p.is_absolute() && exe.contains('/')) || p.starts_with(tree)
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
    let find = |p: u32| table.iter().find(|(q, _, _)| *q == p);
    let Some(&(_, ppid, ref exe)) = find(pid) else { return Verdict::Spare("it is not in the process table".into()) };
    if let Some(why) = host_named(exe) {
        return Verdict::Spare(format!("{why} that stands there by working directory"));
    }
    let built = BUILT_SERVERS.contains(&base_of(exe));
    if exe_in(exe, tree) {
        return Verdict::Stop;
    }
    if built {
        return Verdict::Spare(format!("a server ({}) that is not the tree's build", base_of(exe)));
    }
    if DEV_TOOLS.contains(&base_of(exe)) {
        return Verdict::Stop;
    }
    // Up through its parents, to the first that decides it: a host makes it
    // the host's; one that stood in the tree makes it the tree's. Further up is
    // not asked: every pane has a herdr above it.
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
    lefts
        .into_iter()
        .filter_map(|mut l| {
            let in_tree = pids_in(lsof, Path::new(&l.tree));
            let (mut stop, mut spare) = (Vec::new(), Vec::new());
            for p in std::mem::take(&mut l.pids) {
                match verdict(p, Path::new(&l.tree), &in_tree, table) {
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
/// `None` when no backend answered: a census that failed says nothing about
/// which rows have a seat, so the caller signals and names nothing.
pub fn left_in_finished(
    store: &Store,
    census: Vec<(String, String)>,
    pids: &dyn Fn(&str) -> Option<Vec<u32>>,
    staged: &dyn Fn(&crate::model::Task) -> bool,
) -> Option<Vec<Left>> {
    let mut live = BTreeSet::new();
    let mut keep = crate::base::procs::ancestors(&crate::base::procs::parents());
    for (seat, label) in census {
        live.insert(label.split(" · ").next().unwrap_or_default().trim_start_matches('(').to_string());
        keep.extend(pids(&seat).unwrap_or_default());
    }
    let claims = store.claims();
    let finished = |name: &str| {
        !live.contains(name)
            && !claims.contains_key(name)
            && store.task(name).is_some_and(|t| {
                matches!(t.status(), crate::model::Status::Review | crate::model::Status::Done) && !staged(&t)
            })
    };
    let out = wsp_base::calls::Command::new("lsof").args(["-d", "cwd", "-Fpn"]).output().ok()?;
    let lsof = String::from_utf8_lossy(&out.stdout);
    let found = finished_trees(&lsof, &finished, &keep);
    // The executables are read only for processes there are to sort (`wsp-480`).
    if found.is_empty() {
        return Some(found);
    }
    Some(sorted(&lsof, found, &exes()))
}

/// The resident memory of `pids` in kilobytes.
pub fn rss_kb(pids: &[u32]) -> u64 {
    let list = pids.iter().map(u32::to_string).collect::<Vec<_>>().join(",");
    let Ok(out) = wsp_base::calls::Command::new("ps").args(["-o", "rss=", "-p", &list]).output() else { return 0 };
    String::from_utf8_lossy(&out.stdout).lines().filter_map(|l| l.trim().parse::<u64>().ok()).sum()
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

/// The `cycle.log` line for what ending a tree signalled, so a run read
/// afterwards says which processes a check's end took (`wsp-388`). `None` when
/// nothing was signalled.
pub fn signalled_line(path: &str, term: &[u32], kill: &[u32]) -> Option<String> {
    let list = |pids: &[u32]| pids.iter().map(u32::to_string).collect::<Vec<_>>().join(" ");
    match (term.is_empty(), kill.is_empty()) {
        (true, _) => None,
        (false, true) => Some(format!("stopped in barrier check tree {path}: TERM {}", list(term))),
        (false, false) => Some(format!("stopped in barrier check tree {path}: TERM {}, KILL {}", list(term), list(kill))),
    }
}

/// Take the tree at `dir`, of the repository at `repo`, unless somebody is in
/// it or something is. What its check left running is stopped only once the
/// tree is known to be going; a tree that is kept keeps its servers.
fn take(store: &Store, repo: &Path, dir: &Path, residue: &Residue, standing: &dyn Fn(&Path) -> Option<String>) -> Tree {
    let path = util::contract(dir);
    if let Some(why) = standing(dir) {
        return Tree::Kept { path, why };
    }
    match litter(dir, residue) {
        None => Tree::Kept { path, why: "git could not read it".into() },
        Some(found) if !found.is_empty() => Tree::Kept { path, why: holds_more(&found) },
        Some(_) => {
            // Only a tree that is going: a kept one keeps its servers.
            let (term, kill) = stop_running(dir);
            if let Some(line) = signalled_line(&path, &term, &kill) {
                crate::store::log_line(store, &line);
            }
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
            if in_use(store, &list, at) {
                continue;
            }
            out.push(take(store, &repo, &dir, &residue, standing));
        }
    }
    out
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
    let standing = |dir: &Path| -> Option<String> {
        let dir = util::real(&dir.display().to_string());
        match seen.get_or_init(cmd_checkout::who_is_standing) {
            Ok(rows) => rows
                .iter()
                .filter(|(id, _)| !ended.contains(id))
                .find(|(_, cwd)| util::real(cwd).starts_with(&dir))
                .map(|(id, _)| format!("{id} is standing in it")),
            Err(why) => Some(why.clone()),
        }
    };
    let found = sweep(store, &standing);
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

    /// What a check's end signalled is said in `cycle.log`, by tree and pid.
    #[test]
    fn what_ending_a_tree_signalled_is_one_line_naming_the_tree_and_the_pids() {
        assert_eq!(signalled_line("~/r/.worktrees/x-check-1", &[], &[]), None);
        assert_eq!(
            signalled_line("~/r/.worktrees/x-check-1", &[10, 12], &[]).as_deref(),
            Some("stopped in barrier check tree ~/r/.worktrees/x-check-1: TERM 10 12")
        );
        assert_eq!(
            signalled_line("~/r/.worktrees/x-check-1", &[10, 12], &[12]).as_deref(),
            Some("stopped in barrier check tree ~/r/.worktrees/x-check-1: TERM 10 12, KILL 12")
        );
    }

    /// `wsp-388` where it happened: a seat whose process stands in a check's
    /// tree by cwd, beside a dev server that does too. What `stop_running`
    /// would signal is the server alone. Dropping the seats from the keep-set
    /// (`running_among`) makes this fail.
    #[test]
    fn what_ending_a_check_would_signal_leaves_a_seat_standing_in_its_tree_alone() {
        use crate::place::{Order, Seat};
        use crate::place_compound::Compound;
        use std::os::unix::process::CommandExt;
        let root = std::env::temp_dir().join(format!("wsp-388-{}-{}", std::process::id(), util::epoch_nanos()));
        let tree = root.join("tree");
        std::fs::create_dir_all(&tree).unwrap();
        let place = Compound::at(root.join("seats"));
        let seat: Seat = place.open(&Order { label: "member".into(), ..Order::default() }).unwrap();
        let stood = |group: bool| {
            let mut c = wsp_base::calls::Command::new("sleep");
            c.arg("60").current_dir(&tree).stdin(std::process::Stdio::null());
            if group {
                c.process_group(0);
            }
            c.spawn().unwrap()
        };
        let mut supervisor = stood(true);
        let mut server = stood(false);
        place.record_pid_for_test(&seat, supervisor.id());

        let tree = util::real(&tree.display().to_string());
        let found = running_among(&tree, &[&place as &dyn Place]);
        let _ = supervisor.kill();
        let _ = server.kill();
        let _ = supervisor.wait();
        let _ = server.wait();
        let _ = std::fs::remove_dir_all(&root);

        assert!(found.contains(&server.id()), "the server in the tree is the tree's: {found:?}");
        assert!(!found.contains(&supervisor.id()), "a seat's process was named for stopping: {found:?}");
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

    /// `wsp-478`: the herdr server and a `wsp` stood in a finished tree by
    /// working directory alone and were TERMed.
    #[test]
    fn a_herdr_or_wsp_standing_in_a_finished_tree_is_spared() {
        let table = [proc(31702, 1, "/opt/homebrew/bin/herdr"), proc(23862, 1, "/Users/e/.local/bin/wsp")];
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

    #[test]
    fn what_a_host_started_is_the_hosts() {
        let table = [proc(30, 1, "/opt/homebrew/bin/herdr"), proc(31, 30, "/usr/bin/some-pane")];
        assert!(matches!(judged(31, &[30, 31], &table), Verdict::Spare(_)));
    }

    #[test]
    fn sorting_a_tree_splits_what_it_started_from_what_only_stands_there() {
        let lsof = "p31702\nn/r/.worktrees/c-1\np40\nn/r/.worktrees/c-1/web\n";
        let table = [proc(31702, 1, "/opt/homebrew/bin/herdr"), proc(40, 1, "/usr/local/bin/vite")];
        let lefts = vec![Left { row: "c-1".into(), tree: "/r/.worktrees/c-1".into(), pids: vec![31702, 40], spared: vec![] }];
        let out = sorted(lsof, lefts, &table);
        assert_eq!(out[0].pids, vec![40]);
        assert_eq!(out[0].spared.len(), 1);
        assert_eq!(out[0].spared[0].0, 31702);
        assert!(spared_line(&out[0], 31702, &out[0].spared[0].1).starts_with("spared 31702 in finished row c-1"));
    }

    /// With no backend answering there is nothing to tell a seat from a
    /// stray, so nothing is named.
    #[test]
    fn with_no_backend_answering_nothing_is_named_for_stopping() {
        assert!(seated(&[]).is_none());
        assert!(running_among(Path::new("/"), &[]).is_empty());
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
