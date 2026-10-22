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
//! stands in the tree, and the tree holds nothing beyond `target/` — no
//! tracked change, nothing untracked, nothing ignored. A check reads; it has
//! no business writing, and something written there is kept and named with
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
use crate::store::Store;
use crate::util;

/// The check a directory under `.worktrees/` is the tree of, if it is one.
///
/// A name is only a check's when it parses as one **and** the list it names
/// exists **and** no task has that id: `parse_subject` alone would read a
/// project called `x-check`'s task `x-check-012` as list `x`'s twelfth group.
pub(crate) fn named(store: &Store, name: &str) -> Option<(String, usize)> {
    let (list, at) = crate::barrier::parse_subject(name)?;
    if store.task(name).is_some() || store.worklist(list).is_none() {
        return None;
    }
    Some((list.to_string(), at))
}

/// Whether group `at` of `list` still has a check that is somebody's: one
/// still reading, or a seat not yet ended. A fresh check on the same group is
/// started in the same tree, and its entry is written before its seat exists.
fn in_use(store: &Store, list: &str, at: usize) -> bool {
    store
        .worklist(list)
        .and_then(|w| w.groups().into_iter().nth(at.saturating_sub(1)))
        .is_some_and(|g| g.checks.iter().any(|c| c.open() || c.standing().is_some()))
}

/// What is in a tree besides `target/`, by path — `None` when git cannot read
/// it, which keeps it.
pub(crate) fn litter(dir: &Path) -> Option<Vec<String>> {
    let out = git(dir, &["status", "--porcelain", "--ignored", "--untracked-files=normal"])?;
    Some(
        out.lines()
            .filter_map(|l| l.get(3..))
            .map(|p| p.trim_matches('"').to_string())
            .filter(|p| p != "target" && !p.starts_with("target/"))
            .collect(),
    )
}

/// Take the tree at `dir`, of the repository at `repo`, unless somebody is in
/// it or something is.
fn take(repo: &Path, dir: &Path, standing: &dyn Fn(&Path) -> Option<String>) -> Tree {
    let path = util::contract(dir);
    if let Some(why) = standing(dir) {
        return Tree::Kept { path, why };
    }
    match litter(dir) {
        None => Tree::Kept { path, why: "git could not read it".into() },
        Some(found) if !found.is_empty() => {
            let shown = found.iter().take(3).cloned().collect::<Vec<_>>().join(", ");
            let more = if found.len() > 3 { format!(" and {} more", found.len() - 3) } else { String::new() };
            Tree::Kept { path, why: format!("it holds more than target/: {shown}{more}") }
        }
        Some(_) => Tree::Removed { path, kept: cmd_checkout::remove(repo, dir).kept },
    }
}

/// The repositories check trees can be in: every project's roots, each walked
/// up to the directory holding `.git` — on disk, with no git to run, because
/// the tick asks this every twenty seconds and nearly always finds nothing.
/// A root that is itself a linked tree is skipped; its trunk is a root too.
pub(crate) fn repos(store: &Store) -> Vec<PathBuf> {
    let mut seen = BTreeSet::new();
    let mut out = Vec::new();
    for root in store.projects().iter().flat_map(|p| p.roots.iter()) {
        let mut dir = util::expand(root);
        while !dir.join(".git").exists() {
            match dir.parent() {
                Some(up) => dir = up.to_path_buf(),
                None => break,
            }
        }
        if !dir.join(".git").is_dir() {
            continue;
        }
        if seen.insert(util::real(&dir.display().to_string())) {
            out.push(dir);
        }
    }
    out
}

/// `despawn`'s half: the tree of the check named `name`, taken on the rule.
pub(crate) fn discard(store: &Store, name: &str, standing: &dyn Fn(&Path) -> Option<String>) -> Tree {
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
    take(&repo, &dir, standing)
}

/// Every check tree on disk whose check is nobody's, taken on the rule.
/// `standing` is asked only once there is a tree to ask it about.
pub(crate) fn sweep(store: &Store, standing: &dyn Fn(&Path) -> Option<String>) -> Vec<Tree> {
    let mut out = Vec::new();
    for repo in repos(store) {
        let Ok(entries) = std::fs::read_dir(repo.join(WORKTREES)) else { continue };
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
            out.push(take(&repo, &dir, standing));
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
pub(crate) fn tidy(store: &Store, ended: &[String]) {
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
                crate::cycle::log_line(store, &format!("removed barrier check tree {path}: its check is over{branch}"));
            }
            Tree::Kept { path, why } => {
                let line = format!("kept barrier check tree {path}: {why}");
                if SAID.lock().map(|mut s| s.insert(line.clone())).unwrap_or(true) {
                    crate::cycle::log_line(store, &line);
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
    use std::process::Command;

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
        run(&repo, &["init", "--quiet", "-b", "master"]);
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
        crate::barrier::push(&mut g.checks, checks);
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
            crate::barrier::push(&mut groups[0].checks, checks);
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
}
