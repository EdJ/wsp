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
