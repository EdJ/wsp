//! What a list leaves behind, swept when it completes: `wsp-548`.
//!
//! A sweep used to run in the repository it was typed in, and the barrier's
//! sweep took only the trees of the group it crossed. So on 2026-11-09
//! `~/.local/state/wsp/checks` held 193 entries (38 GB by `du`), the oldest
//! from 2 November, and compound held 15 trees of done rows long after their
//! lists closed. A list that is over has nothing left to wait for, so it is the
//! moment to take what it made.
//!
//! **At a list's completion** (its last barrier passed, or `done`, or `status
//! cancelled`) [`complete`] takes, in this order:
//!
//! 1. the list's check trees, `<root>/.worktrees/<list>-check-<N>`, on
//!    [`check_tree`]'s rule (nobody standing in one, nothing in one beyond
//!    build output);
//! 2. the warm trees under `checks/` and `barrier/` of each project the list
//!    touched, when no other list that has not ended names that project, nobody
//!    holds the tree's lock and nobody stands in it: the next check pays one
//!    cold build for it;
//! 3. the trees of its members that are closed (`done`, or landed and verified),
//!    in **every root of the member's project**, on `sweep_closed`'s rule: the
//!    branch stays, and a tree somebody stands in, one with uncommitted work
//!    and one whose branch the trunk lacks is named and not touched;
//! 4. the scratch of those members (`wsp-546`).
//!
//! and says `swept: N trees, M GB by df` on the list, with the volume's free
//! space read before and after, and each tree it spared with why.
//!
//! `wsp checkout --sweep --all-roots` is the same member-tree sweep by hand
//! across every project root.

use std::path::{Path, PathBuf};
use std::time::Duration;

use crate::check_tree;
use crate::cmd_checkout::{self, Tree};
use crate::model::{Worklist, WorklistStatus};
use crate::store::Store;
use crate::util;

/// What completing a list took, and what it left and why.
#[derive(Default)]
pub(crate) struct Swept {
    /// Trees taken, by path.
    pub trees: Vec<String>,
    /// Trees spared, each as a line naming the tree and why.
    pub kept: Vec<String>,
    /// Scratch directories taken.
    pub scratch: usize,
    /// Free bytes on the state volume before and after, when `df` answered.
    pub free: Option<(u64, u64)>,
}

impl Swept {
    /// The line for the list's log: `swept: N trees, M GB by df`.
    pub(crate) fn line(&self) -> String {
        let n = self.trees.len();
        let trees = format!("{n} tree{}", if n == 1 { "" } else { "s" });
        let freed = match self.free {
            Some((a, b)) => crate::disk::gb(b.saturating_sub(a)),
            None => "size unknown".into(),
        };
        let scratch = match self.scratch {
            0 => String::new(),
            s => format!(", {s} scratch dir{}", if s == 1 { "" } else { "s" }),
        };
        format!("swept: {trees}{scratch}, {freed} by df")
    }

    pub(crate) fn nothing(&self) -> bool {
        self.trees.is_empty() && self.kept.is_empty() && self.scratch == 0
    }
}

/// Complete `w`: take what it left. `busy` answers who is standing in a
/// member's tree, `standing` who is standing anywhere in a path; the caller
/// builds both from the live backends, and a test from what it likes.
pub(crate) fn complete(
    store: &Store,
    w: &Worklist,
    busy: &dyn Fn(&str, &Path) -> Option<String>,
    standing: &dyn Fn(&Path) -> Option<String>,
) -> Swept {
    let mut out = Swept::default();
    let before = crate::disk::free_at(&store.state);

    // 1. the list's own check trees, in every root.
    for tree in check_tree::sweep_of(store, Some(&w.id), standing) {
        match tree {
            Tree::Removed { path, .. } => out.trees.push(path),
            Tree::Kept { path, why } => out.kept.push(format!("kept check tree {path}: {why}")),
            Tree::Absent => {}
        }
    }

    let members: Vec<String> = w.groups().iter().flat_map(|g| g.members.iter().cloned()).collect();
    let mut projects: Vec<String> = members.iter().filter_map(|m| store.task(m).and_then(|t| t.project)).collect();
    projects.sort();
    projects.dedup();

    // 2. the warm trees of projects no other list is on.
    for project in &projects {
        if let Some(other) = other_list_on(store, &w.id, project) {
            out.kept.push(format!("kept the warm check trees of {project}: {other} is still on it"));
            continue;
        }
        warm_trees(store, project, standing, &mut out);
    }

    // 3. the closed members' trees, in each root of their project.
    let tasks = store.tasks();
    let closed: Vec<String> = members
        .iter()
        .filter(|m| {
            store.task(m).is_some_and(|t| t.status() == crate::model::Status::Done)
                || crate::scratch::landed_and_verified(store, &tasks, m)
        })
        .cloned()
        .collect();
    let swept = cmd_checkout::sweep_closed_with(store, &closed, Some(busy), false);
    for id in &swept.removed {
        out.trees.push(id.clone());
    }
    for (id, why) in &swept.kept {
        out.kept.push(format!("kept {id}'s tree: {why}"));
    }
    for b in &swept.branches {
        out.kept.push(format!("branch {b} kept — it has commits the trunk has not"));
    }

    // 4. their scratch.
    let owned = store.state.join("scratch");
    for f in crate::scratch::find(store).into_iter().filter(|f| f.dir.starts_with(&owned) && closed.contains(&f.row)) {
        crate::scratch::remove(&f);
        out.scratch += 1;
    }

    out.free = before.zip(crate::disk::free_at(&store.state));
    out
}

/// A list other than `list` that has not ended and has a member in `project`.
fn other_list_on(store: &Store, list: &str, project: &str) -> Option<String> {
    store
        .worklists()
        .into_iter()
        .filter(|o| o.id != list && matches!(o.status(), WorklistStatus::Running | WorklistStatus::Held | WorklistStatus::Parked))
        .find(|o| {
            o.groups().iter().flat_map(|g| g.members.iter()).any(|m| store.task(m).and_then(|t| t.project).as_deref() == Some(project))
        })
        .map(|o| o.id)
}

/// The warm `tree` of `checks/<project>-<repo>` and `barrier/<project>-<repo>`
/// for each repository the project names, taken unless its lock is held or
/// somebody stands in it.
fn warm_trees(store: &Store, project: &str, standing: &dyn Fn(&Path) -> Option<String>, out: &mut Swept) {
    let Some(p) = store.projects().into_iter().find(|p| p.name == project) else { return };
    let mut seen = std::collections::BTreeSet::new();
    for repo in p.roots.iter().filter_map(|r| check_tree::repo_of(r)) {
        let Some(name) = repo.file_name().map(|n| n.to_string_lossy().into_owned()) else { continue };
        for area in ["checks", "barrier"] {
            let dir = store.state_file(area).join(format!("{project}-{name}"));
            if !seen.insert(dir.clone()) || !dir.join("tree/.git").exists() {
                continue;
            }
            take_warm(&repo, &dir, standing, out);
        }
    }
}

fn take_warm(repo: &Path, dir: &Path, standing: &dyn Fn(&Path) -> Option<String>, out: &mut Swept) {
    let tree: PathBuf = dir.join("tree");
    let path = util::contract(&tree);
    if let Some(why) = standing(&tree) {
        out.kept.push(format!("kept warm tree {path}: {why}"));
        return;
    }
    // Held across the removal, so a check that starts now waits and then finds
    // no tree and makes one.
    let Ok(_lock) = crate::check::TreeLock::take(dir.join("lock"), Duration::ZERO) else {
        out.kept.push(format!("kept warm tree {path}: a check holds its lock"));
        return;
    };
    cmd_checkout::remove(repo, &tree);
    match tree.exists() {
        true => out.kept.push(format!("kept warm tree {path}: it would not go")),
        false => out.trees.push(path),
    }
}

/// [`complete`] with the live backends, said on the list's log and in
/// `cycle.log`; the list is saved by the caller, which has it open.
pub(crate) fn finish(store: &Store, w: &mut Worklist) -> Swept {
    // Asked only once there is a tree to ask about: most lists have none left.
    let occupied = std::cell::OnceCell::new();
    let seen = std::cell::OnceCell::new();
    let standing = |dir: &Path| -> Option<String> {
        let dir = util::real(&dir.display().to_string());
        match seen.get_or_init(cmd_checkout::who_is_standing) {
            Ok(rows) => rows.iter().find(|(_, cwd)| util::real(cwd).starts_with(&dir)).map(|(id, _)| format!("{id} is standing in it")),
            Err(why) => Some(why.clone()),
        }
    };
    let out = complete(store, w, &|t, d| occupied.get_or_init(|| cmd_checkout::Occupied::now(store)).of(t, d), &standing);
    if !out.nothing() {
        w.log(&out.line());
        for k in &out.kept {
            w.log(k);
        }
        crate::store::log_line(store, &format!("{}: {}", w.id, out.line()));
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{Group, Project, Status, Task};
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

    fn repo(env: &util::Isolated, name: &str) -> PathBuf {
        let repo = env.path(name);
        std::fs::create_dir_all(&repo).unwrap();
        crate::testkit::repo(&repo);
        std::fs::write(repo.join(".gitignore"), "target/\n.worktrees/\n").unwrap();
        run(&repo, &["add", ".gitignore"]);
        run(&repo, &["commit", "--quiet", "-m", "first"]);
        repo
    }

    fn tree(repo: &Path, name: &str) -> PathBuf {
        let dir = repo.join(".worktrees").join(name);
        run(repo, &["worktree", "add", "--quiet", "-b", name, &dir.display().to_string(), "master"]);
        dir
    }

    /// A project of two roots, a list `run` of members `p-001` and `p-002`
    /// (both done), a tree of each in each root, and a check tree.
    fn world() -> (util::Isolated, Store, Vec<PathBuf>) {
        let env = util::isolated("list-sweep");
        let a = repo(&env, "a");
        let b = repo(&env, "b");
        let store = Store::open();
        let mut p = Project::new("p");
        p.roots = vec![a.display().to_string(), b.display().to_string()];
        store.save_project(&p).unwrap();
        for id in ["p-001", "p-002"] {
            let mut t = Task::new("a row", id);
            t.project = Some("p".into());
            t.set_status(Status::Done);
            store.save_task(&t).unwrap();
        }
        let mut w = Worklist::new("run", "a run");
        let mut g = Group::default();
        g.members = vec!["p-001".into(), "p-002".into()];
        w.set_groups(&[g]);
        w.set_status(WorklistStatus::Done);
        store.save_worklist(&w).unwrap();
        (env, store, vec![a, b])
    }

    fn free(_: &str, _: &Path) -> Option<String> {
        None
    }
    fn nobody(_: &Path) -> Option<String> {
        None
    }

    /// The point of it: a completed list takes its check tree and its closed
    /// members' trees in both roots, and says so with the df figures.
    #[test]
    fn a_completed_list_takes_its_check_tree_and_its_members_trees_in_every_root() {
        let (_env, store, roots) = world();
        let check = tree(&roots[0], "run-check-1");
        let one_a = tree(&roots[0], "p-001");
        let one_b = tree(&roots[1], "p-001");
        let two_b = tree(&roots[1], "p-002");
        let w = store.worklist("run").unwrap();

        let out = complete(&store, &w, &free, &nobody);
        for gone in [&check, &one_a, &one_b, &two_b] {
            assert!(!gone.exists(), "{} is still there", gone.display());
        }
        assert_eq!(out.trees.len(), 4, "{:?}", out.trees);
        assert!(out.kept.is_empty(), "{:?}", out.kept);
        assert!(out.line().starts_with("swept: 4 trees, "), "{}", out.line());
        assert!(out.line().ends_with(" by df"), "{}", out.line());
        assert!(complete(&store, &w, &free, &nobody).nothing(), "the second sweep found something");
    }

    /// A dirty tree and an occupied one are named and not touched, and the rest
    /// still go.
    #[test]
    fn a_dirty_tree_and_an_occupied_one_are_named_and_left() {
        let (_env, store, roots) = world();
        let dirty = tree(&roots[0], "p-001");
        std::fs::write(dirty.join("notes.txt"), "uncommitted\n").unwrap();
        let held = tree(&roots[1], "p-002");
        let w = store.worklist("run").unwrap();

        let standing = |_: &str, d: &Path| d.ends_with("p-002").then(|| "cpd-7 is standing in it".to_string());
        let out = complete(&store, &w, &standing, &nobody);
        assert!(dirty.join("notes.txt").exists() && held.exists(), "a tree was taken");
        assert!(out.trees.is_empty(), "{:?}", out.trees);
        assert!(out.kept.iter().any(|k| k.contains("p-001") && k.contains("uncommitted")), "{:?}", out.kept);
        assert!(out.kept.iter().any(|k| k.contains("p-002") && k.contains("cpd-7")), "{:?}", out.kept);
    }

    /// A branch with commits the trunk lacks keeps its tree.
    #[test]
    fn a_tree_whose_branch_is_unlanded_stays() {
        let (_env, store, roots) = world();
        let dir = tree(&roots[0], "p-001");
        std::fs::write(dir.join("work.txt"), "work\n").unwrap();
        run(&dir, &["add", "work.txt"]);
        run(&dir, &["commit", "--quiet", "-m", "work"]);
        let w = store.worklist("run").unwrap();
        let out = complete(&store, &w, &free, &nobody);
        assert!(dir.exists());
        assert!(out.kept.iter().any(|k| k.contains("p-001")), "{:?}", out.kept);
    }

    /// A member still being worked is not closed, so its tree stays.
    #[test]
    fn a_member_not_closed_keeps_its_tree() {
        let (_env, store, roots) = world();
        let dir = tree(&roots[0], "p-001");
        let mut t = store.task("p-001").unwrap();
        t.set_status(Status::Doing);
        store.save_task(&t).unwrap();
        let w = store.worklist("run").unwrap();
        complete(&store, &w, &free, &nobody);
        assert!(dir.exists());
    }
}
