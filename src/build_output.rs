//! A landed row's build output, dropped once nobody is working in it: `wsp-327`.
//!
//! `wsp done` takes a row's tree (`wsp-305`), and `done` is the person's
//! bookkeeping: rows sit at `review` for days, about 126 of them on 2026-11-01,
//! each tree holding gigabytes of `target/`. That is what filled the machine to
//! 3.4 GB free. The tree is not the cost and may be wanted — the person reads
//! the row there — so only the build output goes, and a reopened row pays one
//! cold build for it.
//!
//! **The rule**, read off the disk by the daemon's tick so it reaches a row
//! however its agent went: the row is at `review` or `done`, holds no claim,
//! nobody is standing in the tree (a verifier stands in its member's tree), and
//! the tree's branch has nothing the trunk lacks — the work is landed. The row's
//! own tree and its legs' trees (`<row>.<leg>`) are taken alike. A tree that
//! fails any of it keeps its build output and `cycle.log` says why, once per
//! daemon process.
//!
//! Idempotent for the reason `check_tree` is: a dropped `target/` is not there
//! to find again.

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};
use std::sync::Mutex;

use crate::check_tree;
use crate::cmd_checkout::{self, WORKTREES};
use crate::model::Status;
use crate::store::Store;
use crate::util;

pub(crate) enum Build {
    Dropped { path: String },
    Kept { path: String, why: String },
}

/// Drop the build output of every released, landed row's tree. `busy` answers
/// for the world outside git — who is standing in a tree, who holds a claim —
/// and is asked only once a tree is found with a `target/` to lose.
pub(crate) fn sweep(store: &Store, busy: &dyn Fn(&str, &Path) -> Option<String>) -> Vec<Build> {
    let mut out = Vec::new();
    for repo in check_tree::repos(store) {
        let Ok(entries) = std::fs::read_dir(repo.join(WORKTREES)) else { continue };
        let mut names: Vec<String> = entries.flatten().map(|e| e.file_name().to_string_lossy().into_owned()).collect();
        names.sort();
        for name in names {
            let dir = repo.join(WORKTREES).join(&name);
            let target = dir.join("target");
            if !dir.join(".git").exists() || !std::fs::symlink_metadata(&target).is_ok_and(|m| m.is_dir()) {
                continue;
            }
            let row = crate::leg_tree::row_of(&name);
            let Some(task) = store.task(row) else { continue };
            if !matches!(task.status(), Status::Review | Status::Done) {
                continue;
            }
            let path = util::contract(&dir);
            if let Some(why) = busy(row, &dir) {
                out.push(Build::Kept { path, why });
                continue;
            }
            match unlanded(&repo, &dir) {
                Some(why) => out.push(Build::Kept { path, why }),
                None => match std::fs::remove_dir_all(&target) {
                    Ok(()) => out.push(Build::Dropped { path }),
                    Err(e) => out.push(Build::Kept { path, why: format!("its target/ would not go: {e}") }),
                },
            }
        }
    }
    out
}

/// Why the tree at `dir` is not landed, or `None` when its branch holds nothing
/// the trunk has not got.
fn unlanded(repo: &Path, dir: &Path) -> Option<String> {
    let trunk: PathBuf = cmd_checkout::trunk(repo)?;
    let Some(onto) = cmd_checkout::trunk_branch(&trunk) else { return Some("the trunk is on a detached HEAD".into()) };
    let Some(branch) = cmd_checkout::trunk_branch(dir) else { return Some("the tree is on a detached HEAD".into()) };
    let commits = cmd_checkout::ahead(&trunk, &onto, &branch);
    (!commits.is_empty()).then(|| format!("its branch {branch} has {} commit(s) {onto} has not — not landed", commits.len()))
}

static SAID: Mutex<BTreeSet<String>> = Mutex::new(BTreeSet::new());

/// The tick's half: [`sweep`] with the live panes and claims as `busy`, and what
/// it did in `cycle.log`.
///
/// Not under test, for `check_tree::tidy`'s reason; the tests drive [`sweep`].
pub(crate) fn tidy(store: &Store) {
    if cfg!(test) {
        return;
    }
    let occupied = std::cell::OnceCell::new();
    let busy = |row: &str, dir: &Path| occupied.get_or_init(|| cmd_checkout::Occupied::now(store)).of(row, dir);
    for build in sweep(store, &busy) {
        match build {
            Build::Dropped { path } => crate::cycle::log_line(store, &format!("dropped build output of {path}: its row is landed and released")),
            Build::Kept { path, why } => {
                let line = format!("kept build output of {path}: {why}");
                if SAID.lock().map(|mut s| s.insert(line.clone())).unwrap_or(true) {
                    crate::cycle::log_line(store, &line);
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{Project, Task};
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

    /// A scratch repository under the test's own directory with a project
    /// rooted at it and a task `p-001` in the given status.
    fn scratch(name: &str, status: Status) -> (util::Isolated, Store, PathBuf) {
        let env = util::isolated(&format!("build-output-{name}"));
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
        let mut t = Task::new("a row", "p-001");
        t.project = Some("p".into());
        t.set_status(status);
        store.save_task(&t).unwrap();
        (env, store, repo)
    }

    fn tree_with_build(repo: &Path, name: &str) -> PathBuf {
        let dir = repo.join(WORKTREES).join(name);
        run(repo, &["worktree", "add", "--quiet", "-b", name, &dir.display().to_string(), "master"]);
        std::fs::create_dir_all(dir.join("target/debug")).unwrap();
        std::fs::write(dir.join("target/debug/big"), "build output\n").unwrap();
        dir
    }

    fn free(_: &str, _: &Path) -> Option<String> {
        None
    }

    /// The point of it: a landed row at review keeps its tree and loses its
    /// `target/`, its legs' trees the same, and a second sweep finds nothing.
    #[test]
    fn a_landed_row_at_review_loses_its_build_output_and_keeps_its_tree() {
        let (_env, store, repo) = scratch("review", Status::Review);
        let row = tree_with_build(&repo, "p-001");
        let leg = tree_with_build(&repo, "p-001.scout");
        std::fs::write(row.join("notes.txt"), "uncommitted work stays\n").unwrap();

        let out = sweep(&store, &free);
        assert_eq!(out.iter().filter(|b| matches!(b, Build::Dropped { .. })).count(), 2);
        assert!(!row.join("target").exists() && !leg.join("target").exists(), "a target/ is still there");
        assert!(row.join(".git").exists() && leg.join(".git").exists(), "a tree went");
        assert!(row.join("notes.txt").exists());
        assert!(sweep(&store, &free).is_empty(), "the second sweep found something to do");
    }

    /// A row still being worked, a tree somebody stands in, a row whose branch
    /// holds commits the trunk lacks, and a tree no row owns each keep theirs.
    #[test]
    fn build_output_stays_while_the_row_is_worked_occupied_unlanded_or_nobodys() {
        let (_env, store, repo) = scratch("kept", Status::Doing);
        let row = tree_with_build(&repo, "p-001");
        let stranger = tree_with_build(&repo, "p-009");
        assert!(sweep(&store, &free).is_empty(), "a row being worked lost its build output");

        let mut t = store.task("p-001").unwrap();
        t.set_status(Status::Review);
        store.save_task(&t).unwrap();
        let standing = |_: &str, _: &Path| Some("cpd-7 is standing in it".to_string());
        match sweep(&store, &standing).as_slice() {
            [Build::Kept { why, .. }] => assert!(why.contains("cpd-7"), "{why}"),
            _ => panic!("a tree somebody stands in was not kept"),
        }

        std::fs::write(row.join("work.txt"), "committed, not landed\n").unwrap();
        run(&row, &["add", "work.txt"]);
        run(&row, &["commit", "--quiet", "-m", "work"]);
        match sweep(&store, &free).as_slice() {
            [Build::Kept { why, .. }] => assert!(why.contains("not landed"), "{why}"),
            _ => panic!("an unlanded row lost its build output"),
        }
        assert!(row.join("target").exists() && stranger.join("target").exists());

        run(&repo, &["merge", "--quiet", "--ff-only", "p-001"]);
        assert!(matches!(sweep(&store, &free).as_slice(), [Build::Dropped { .. }]));
        assert!(!row.join("target").exists() && stranger.join("target").exists());
    }

    /// `done` rows too: `wsp done` keeps a tree it cannot take, and its build
    /// output should not outlive the row's review either.
    #[test]
    fn a_done_row_loses_its_build_output_too() {
        let (_env, store, repo) = scratch("done", Status::Done);
        let row = tree_with_build(&repo, "p-001");
        assert!(matches!(sweep(&store, &free).as_slice(), [Build::Dropped { .. }]));
        assert!(!row.join("target").exists());
    }
}
