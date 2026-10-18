//! A sibling's tree, and the merge back: `docs/legs.md` *Trees*, wsp-297.
//!
//! **A leg that runs alone stands in the row's tree. A leg that has a sibling
//! gets a tree of its own**, `<trunk>/.worktrees/<task>.<leg>` on a branch of
//! the same name, cut from the row's branch at its tip. Which legs have
//! siblings is read off the graph ([`has_siblings`]) and never off what happens
//! to be running, so the tree a leg stands in does not depend on timing and
//! neither does the merge.
//!
//! This module is the git half and nothing else. It is called from the leg's
//! spawn (legs-seat, legs-run), from `leg done`, from `despawn` and from the
//! tick, and it writes no record: what a call returns is what the caller puts
//! on the entry (`tree <name>`) or in a brief.
//!
//! - [`open`] makes the tree, off the row's tip.
//! - [`join`] is the merge back. Before a leg opens, each leg it waits on that
//!   has a tree is merged into the row's branch, in the row's tree, in
//!   `after:` order. A conflict is aborted so the row's tree stays clean at a
//!   commit, and the rest is handed to the join's brief ([`Joined::brief`]).
//! - [`overlaps`] is the early warning on a sibling's `leg done`.
//! - [`discard`], [`sweep`] and [`tidy`] take a tree when its leg is over, by
//!   `check_tree`'s rule.
//!
//! # The choices the doc left open (wsp-297, v1)
//!
//! - **Merged means an ancestor of the row's branch**, not of the trunk. A
//!   leg's work lives on the row's branch until `wsp land`, so the branch is
//!   deleted with `-D` once that is checked here; `branch -d` would compare with
//!   the trunk and keep every one of them.
//! - **A tree with a live neighbour is not merged into.** The doc assumes no
//!   agent stands in the row's tree when a join opens. [`join`] checks it
//!   instead of trusting it: a running leg with no tree of its own, or a row
//!   tree with uncommitted changes, holds the whole merge back and says so. The
//!   skip case (A running, B skipped, J naming only B) is closed by
//!   `legs::ready`, which waits on every leg upstream; a governor adding a
//!   sibling beside a leg already running in the row's tree is what this catches.
//! - **A branch that is gone merged long ago.** A leg's tree is only discarded
//!   after its branch is in the row's, so a missing branch has nothing to merge.
//! - **Fast-forward is allowed.** The first merge into a row whose tip has not
//!   moved is a fast-forward, as `git merge --no-edit` does, and the brief
//!   reports the tip it left either way.
//! - **A row whose task is not in the store, or closed, is not swept here.**
//!   A closed row's leg trees go with the row's own, through `wsp checkout
//!   --sweep` (`cmd_checkout::stale` reads `<task>.<leg>` as the row's).

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};
use std::sync::Mutex;

use crate::cmd_checkout::{self, Tree, WORKTREES};
use crate::cmd_verify::{self, git, git_ok};
use crate::legs::{self, Leg, State};
use crate::model::Task;
use crate::store::Store;
use crate::util;

/// The tree and the branch a leg of `task` works in when it has siblings.
pub(crate) fn tree_name(task: &str, leg: &str) -> String {
    format!("{task}.{leg}")
}

/// The row a tree or branch name belongs to: everything before the first `.`.
///
/// A task id carries no `.` (`legs::name_ok` keeps leg names clear of one too),
/// so a name with a dot is a leg's and a name without one is returned as it is.
/// One place for `finished`, `naming`, `doctor` and the sweep to agree.
pub(crate) fn row_of(name: &str) -> &str {
    name.split_once('.').map_or(name, |(row, _)| row)
}

/// The legs that run beside `name`: neither waits on the other, by any path.
pub(crate) fn siblings(legs: &[Leg], name: &str) -> Vec<String> {
    let up = legs::upstream(legs, name);
    let down = legs::downstream(legs, name);
    legs.iter()
        .map(|l| l.name.as_str())
        .filter(|n| *n != name && !up.contains(*n) && !down.contains(*n))
        .map(str::to_string)
        .collect()
}

/// Whether `name` gets a tree of its own.
pub(crate) fn has_siblings(legs: &[Leg], name: &str) -> bool {
    !siblings(legs, name).is_empty()
}

/// The leg a directory under `.worktrees/` is the tree of, if it is one: the
/// task exists in the store and has a leg of that name.
pub(crate) fn named(store: &Store, name: &str) -> Option<(Task, Leg)> {
    let (row, leg) = name.split_once('.')?;
    let task = store.task(row)?;
    let found = legs::legs(&task).into_iter().find(|l| l.name == leg)?;
    Some((task, found))
}

// ---- opening --------------------------------------------------------------------

/// A tree made for a leg.
#[derive(Debug)]
pub(crate) struct Opened {
    /// The tree, as `~/…`; also the branch's name's directory. The entry's
    /// `tree` clause is [`Opened::name`].
    pub path: String,
    #[cfg_attr(not(test), allow(dead_code))] // read by the tests
    pub dir: PathBuf,
    /// The branch, and the name the entry records.
    pub name: String,
    /// The commit it was cut at, short.
    #[cfg_attr(not(test), allow(dead_code))] // read by the tests
    pub from: String,
}

/// Make the tree for `leg` of `task` off the row's branch at its tip.
///
/// `root` is a project root, as `cmd_checkout::tree_for` takes. The row must
/// have a tree already: it is what the leg is cut from, and a row with none has
/// no tip to say. An existing branch of the leg's name is reused, so a leg that
/// is opened again finds the commits it made.
pub(crate) fn open(root: &str, task: &str, leg: &str) -> Result<Opened, String> {
    let root = util::expand(root);
    let repo = crate::cmd_verify::toplevel(&root).ok_or_else(|| format!("{} is not in a git repository", root.display()))?;
    let trunk = cmd_checkout::trunk(&repo).ok_or("the repository has no main working tree")?;
    let row_dir = cmd_checkout::checkout_dir(&trunk, task);
    if !row_dir.join(".git").exists() {
        return Err(format!("{task} has no tree to cut {} from — `wsp checkout {task}`", tree_name(task, leg)));
    }
    let row_branch = cmd_checkout::trunk_branch(&row_dir).ok_or_else(|| format!("{task}'s tree is on a detached HEAD"))?;
    let name = tree_name(task, leg);
    let dir = trunk.join(WORKTREES).join(&name);
    cmd_checkout::ensure(&repo, &dir, &name, &row_branch)?;
    let from = git(&dir, &["rev-parse", "--short", "HEAD"]).map(|s| s.trim().to_string()).unwrap_or_default();
    Ok(Opened { path: util::contract(&dir), dir, name, from })
}

// ---- opening a leg, from the spawn -----------------------------------------------

/// Where a leg's seat stands and what it was told on the way there.
#[derive(Debug, Default, PartialEq, Eq)]
pub(crate) struct Prepared {
    /// The tree to open in, as `~/…`, when it is not the row's.
    pub cwd: Option<String>,
    /// The tree's name, for the entry's `tree` clause.
    pub tree: Option<String>,
    /// The join's account, for the entry and so the brief.
    pub join: Option<String>,
}

/// The cap on the join's account, as an order's.
const JOIN_TEXT: usize = legs::TEXT_CAP;

/// What a leg's spawn does before its seat opens: merge what the leg waits on
/// into the row's branch, and cut a tree of its own if it has siblings.
///
/// `row_cwd` is the row's tree, which the spawn has just made or found, and
/// `root` the project root the trees are cut in. A leg that is not ready, or is
/// already running, is left to `leg_seat::open` and to its own record: a leg that
/// was opened before reopens in the tree its entry names, and nothing is merged
/// twice. An `Err` is a tree that could not be made, and the leg must not open
/// in the row's tree instead: that is the tree its sibling's merge goes into.
pub(crate) fn prepare(store: &Store, root: &str, row_cwd: &str, task: &str, name: &str) -> Result<Prepared, String> {
    let Some(t) = store.find_task(task) else { return Ok(Prepared::default()) };
    let all = legs::legs(&t);
    let Some(leg) = all.iter().find(|l| l.name == name) else { return Ok(Prepared::default()) };
    match leg.state {
        State::Running => {
            return Ok(match &leg.tree {
                Some(tree) => Prepared { cwd: Some(tree_path(row_cwd, tree)), ..Prepared::default() },
                None => Prepared::default(),
            });
        }
        State::Pending if legs::ready(&all).iter().any(|r| r == name) => {}
        _ => return Ok(Prepared::default()),
    }
    let row_dir = util::expand(row_cwd);
    // A merge or a sibling's tree both need the row's own tree. The spawn falls
    // back to the trunk checkout when the row's tree could not be made, and a
    // merge there would put the legs' work on master unlanded.
    if has_siblings(&all, name) || !inbound(&all, name).is_empty() {
        require_row_tree(root, row_cwd, task)?;
    }
    let joined = join(&row_dir, &all, name);
    let mut lines: Vec<String> = joined.brief().into_iter().collect();
    // What the legs it waits on both touched, read from their branches.
    let waits: Vec<(String, String)> = inbound(&all, name);
    let mut told = BTreeSet::new();
    for (from, _) in &waits {
        if let Some(line) = overlap_line(from, &overlaps(&row_dir, &all, from)) {
            if told.insert(line.clone()) {
                lines.push(line);
            }
        }
    }
    let mut join_text = lines.join("\n");
    if join_text.chars().count() > JOIN_TEXT {
        join_text = join_text.chars().take(JOIN_TEXT - 1).collect::<String>() + "…";
    }
    let join_text = Some(join_text).filter(|j| !j.is_empty());
    if !has_siblings(&all, name) {
        return Ok(Prepared { cwd: None, tree: None, join: join_text });
    }
    let o = open(root, task, name)?;
    Ok(Prepared { cwd: Some(o.path), tree: Some(o.name), join: join_text })
}

/// `row_cwd` must be the row's own tree under the trunk's `.worktrees/`, not the
/// trunk checkout itself or any other directory.
fn require_row_tree(root: &str, row_cwd: &str, task: &str) -> Result<(), String> {
    let root = util::expand(root);
    let own = cmd_verify::toplevel(&root)
        .and_then(|repo| cmd_checkout::trunk(&repo))
        .map(|trunk| cmd_checkout::checkout_dir(&trunk, task))
        .filter(|dir| dir.join(".git").exists());
    let here = util::expand(row_cwd);
    let same = own.as_ref().is_some_and(|dir| match (dir.canonicalize(), here.canonicalize()) {
        (Ok(a), Ok(b)) => a == b,
        _ => false,
    });
    if same {
        return Ok(());
    }
    Err(format!(
        "{task} has no tree of its own ({} is not it) — a leg's merge and a sibling's tree are cut from the row's tree; `wsp checkout {task}`",
        util::contract(&here)
    ))
}

/// A tree's path beside the row's: both are directories under `.worktrees/`.
fn tree_path(row_cwd: &str, tree: &str) -> String {
    let row = util::expand(row_cwd);
    let path = row.parent().map(|p| p.join(tree)).unwrap_or_else(|| PathBuf::from(tree));
    util::contract(&path)
}

/// Put what [`prepare`] did onto the leg's entry, under the store lock.
pub(crate) fn record(store: &Store, task: &str, name: &str, p: &Prepared) {
    if p.tree.is_none() && p.join.is_none() {
        return;
    }
    store.locked(|| {
        let Some(mut t) = store.find_task(task) else { return };
        let mut all = legs::legs(&t);
        let Some(leg) = all.iter_mut().find(|l| l.name == name) else { return };
        leg.tree = p.tree.clone().or(leg.tree.take());
        leg.join = p.join.clone().or(leg.join.take());
        legs::write(&mut t, &all);
        let _ = store.save_task(&t);
    });
}

/// `leg done`'s line for a sibling: the paths it changed that another sibling's
/// branch has also changed. `dir` is any directory of the repository.
pub(crate) fn overlap_for(dir: &Path, task: &str, all: &[Leg], leg: &str) -> Option<String> {
    let trunk = cmd_checkout::trunk(dir)?;
    let row_dir = cmd_checkout::checkout_dir(&trunk, task);
    row_dir.join(".git").exists().then_some(())?;
    overlap_line(leg, &overlaps(&row_dir, all, leg))
}

// ---- the merge back --------------------------------------------------------------

/// One branch merged into the row's.
#[derive(Debug, PartialEq, Eq)]
pub(crate) struct Merge {
    pub leg: String,
    pub branch: String,
    /// The row's tip after it, short.
    pub tip: String,
}

/// A merge that did not go, and was aborted.
#[derive(Debug, PartialEq, Eq)]
pub(crate) struct Conflict {
    pub leg: String,
    pub branch: String,
    pub files: Vec<String>,
}

/// What a join's merge did.
#[derive(Debug, Default, PartialEq, Eq)]
pub(crate) struct Joined {
    pub merged: Vec<Merge>,
    pub conflict: Option<Conflict>,
    /// `(leg, branch)` still to merge, which the join does first, in the tree it
    /// opened in. After a conflict, the branches that were not tried.
    pub waiting: Vec<(String, String)>,
    /// Why nothing was merged at all: someone is in the row's tree, or it holds
    /// uncommitted changes. Every branch is then in `waiting`.
    pub held: Option<String>,
}

impl Joined {
    /// Nothing for the join to do by hand.
    #[cfg(test)]
    pub(crate) fn clean(&self) -> bool {
        self.conflict.is_none() && self.waiting.is_empty() && self.held.is_none()
    }

    /// The block the join's brief carries: the merges made, and anything left.
    /// `None` when the join waits on no tree of its own.
    pub(crate) fn brief(&self) -> Option<String> {
        if self.merged.is_empty() && self.conflict.is_none() && self.waiting.is_empty() && self.held.is_none() {
            return None;
        }
        let mut out = Vec::new();
        for m in &self.merged {
            out.push(format!("merged {} ({}) into the row's branch; it is at {}", m.leg, m.branch, m.tip));
        }
        if let Some(why) = &self.held {
            out.push(format!("nothing was merged: {why}"));
        }
        if let Some(c) = &self.conflict {
            let files = match c.files.is_empty() {
                true => "files git did not name".to_string(),
                false => c.files.join(", "),
            };
            out.push(format!("CONFLICT merging {} ({}), aborted so the row's tree is clean: {files}", c.leg, c.branch));
        }
        if !self.waiting.is_empty() {
            let names = self.waiting.iter().map(|(l, b)| format!("{l} ({b})")).collect::<Vec<_>>().join(", ");
            out.push(format!("still to merge, first, with `git merge --no-edit <branch>` in your tree: {names}"));
        }
        Some(out.join("\n"))
    }
}

/// Whether `branch` exists.
fn exists(dir: &Path, branch: &str) -> bool {
    git(dir, &["rev-parse", "--verify", "--quiet", &format!("refs/heads/{branch}")]).is_some()
}

/// Whether everything on `branch` is already on `into`.
fn contained(dir: &Path, branch: &str, into: &str) -> bool {
    git(dir, &["merge-base", "--is-ancestor", branch, into]).is_some()
}

fn tip(dir: &Path) -> String {
    git(dir, &["rev-parse", "--short", "HEAD"]).map(|s| s.trim().to_string()).unwrap_or_default()
}

/// The legs `join` waits on that have a tree of their own, in `after:` order.
pub(crate) fn inbound(legs: &[Leg], join: &str) -> Vec<(String, String)> {
    let Some(at) = legs.iter().position(|l| l.name == join) else { return Vec::new() };
    legs::waits(legs)[at]
        .iter()
        .filter_map(|n| legs.iter().find(|l| l.name == *n))
        .filter_map(|l| l.tree.clone().map(|t| (l.name.clone(), t)))
        .collect()
}

/// Merge the branches `join` waits on into the row's branch, in `row_dir`.
///
/// Called before the join opens, under the caller's own idempotence (the join is
/// `pending` until it is written `running`), and safe to call again: a branch
/// already in the row's is passed over, so a retry after a held merge does the
/// rest and a second call after a clean one does nothing.
///
/// **The row's tree is left clean at a commit.** A merge that conflicts is
/// aborted before this returns; nothing is ever left half-merged for the next
/// agent to find.
pub(crate) fn join(row_dir: &Path, legs: &[Leg], join: &str) -> Joined {
    let mut out = Joined::default();
    let wanted: Vec<(String, String)> =
        inbound(legs, join).into_iter().filter(|(_, b)| exists(row_dir, b)).collect();
    let Some(row) = cmd_checkout::trunk_branch(row_dir) else {
        out.held = Some("the row's tree is on a detached HEAD".into());
        out.waiting = wanted;
        return out;
    };
    let wanted: Vec<(String, String)> = wanted.into_iter().filter(|(_, b)| !contained(row_dir, b, &row)).collect();
    if wanted.is_empty() {
        return out;
    }
    // Somebody standing in the row's tree: a running leg with no tree of its own.
    // The graph rules make this unreachable for a record built by the run; a
    // governor adding a sibling beside a leg that is already running is how it
    // happens.
    if let Some(l) = legs.iter().find(|l| l.name != join && l.state == State::Running && l.tree.is_none()) {
        out.held = Some(format!("{} is running in the row's tree", l.name));
        out.waiting = wanted;
        return out;
    }
    let dirty = cmd_checkout::uncommitted(row_dir);
    if dirty > 0 {
        out.held = Some(format!("the row's tree has {dirty} uncommitted path(s)"));
        out.waiting = wanted;
        return out;
    }
    let mut rest = wanted.into_iter();
    while let Some((leg, branch)) = rest.next() {
        match git_ok(row_dir, &["merge", "--no-edit", &branch]) {
            Ok(()) => out.merged.push(Merge { leg, branch, tip: tip(row_dir) }),
            Err(_) => {
                let files = git(row_dir, &["diff", "--name-only", "--diff-filter=U"])
                    .map(|s| s.lines().map(str::to_string).filter(|l| !l.is_empty()).collect::<Vec<_>>())
                    .unwrap_or_default();
                // Whatever git left: a conflicted merge, or a refusal that began
                // nothing. `--abort` has nothing to do for the second.
                let _ = git_ok(row_dir, &["merge", "--abort"]);
                out.waiting = rest.collect();
                out.conflict = Some(Conflict { leg, branch, files });
                break;
            }
        }
    }
    out
}

// ---- the early warning --------------------------------------------------------------

/// The paths `branch` has changed since it left `row`.
fn changed(dir: &Path, row: &str, branch: &str) -> BTreeSet<String> {
    git(dir, &["diff", "--name-only", &format!("{row}...{branch}")])
        .map(|s| s.lines().map(str::to_string).filter(|l| !l.is_empty()).collect())
        .unwrap_or_default()
}

/// For `leg`, each sibling that has also changed some of the paths it has, with
/// the paths. Read from the branches, so it needs no tree: `row_dir` only says
/// which repository and which row branch.
pub(crate) fn overlaps(row_dir: &Path, legs: &[Leg], leg: &str) -> Vec<(String, Vec<String>)> {
    let Some(row) = cmd_checkout::trunk_branch(row_dir) else { return Vec::new() };
    let Some(mine) = legs.iter().find(|l| l.name == leg).and_then(|l| l.tree.clone()) else { return Vec::new() };
    let mine = changed(row_dir, &row, &mine);
    if mine.is_empty() {
        return Vec::new();
    }
    let mut out = Vec::new();
    for sib in siblings(legs, leg) {
        let Some(branch) = legs.iter().find(|l| l.name == sib).and_then(|l| l.tree.clone()) else { continue };
        let both: Vec<String> = changed(row_dir, &row, &branch).intersection(&mine).cloned().collect();
        if !both.is_empty() {
            out.push((sib, both));
        }
    }
    out
}

/// The line `leg done` prints, and the join's brief carries.
pub(crate) fn overlap_line(leg: &str, found: &[(String, Vec<String>)]) -> Option<String> {
    if found.is_empty() {
        return None;
    }
    let each: Vec<String> = found
        .iter()
        .map(|(sib, paths)| {
            let shown = paths.iter().take(5).cloned().collect::<Vec<_>>().join(", ");
            let more = if paths.len() > 5 { format!(" and {} more", paths.len() - 5) } else { String::new() };
            format!("{sib}: {shown}{more}")
        })
        .collect();
    Some(format!("{leg} changed paths a sibling has also changed — {}", each.join("; ")))
}

// ---- taking a tree away ----------------------------------------------------------------

/// Take the tree of a finished leg, on `check_tree`'s rule and one more: its
/// branch is in the row's.
fn take(repo: &Path, dir: &Path, name: &str, row_dir: &Path, standing: &dyn Fn(&Path) -> Option<String>) -> Tree {
    let path = util::contract(dir);
    if let Some(why) = standing(dir) {
        return Tree::Kept { path, why };
    }
    let Some(row) = cmd_checkout::trunk_branch(row_dir) else {
        return Tree::Kept { path, why: "the row's tree is on a detached HEAD".into() };
    };
    if !contained(repo, name, &row) {
        return Tree::Kept { path, why: format!("its branch has commits {row} has not got — merged at the join that waits on it") };
    }
    match crate::check_tree::litter(dir) {
        None => Tree::Kept { path, why: "git could not read it".into() },
        Some(found) if !found.is_empty() => {
            let shown = found.iter().take(3).cloned().collect::<Vec<_>>().join(", ");
            let more = if found.len() > 3 { format!(" and {} more", found.len() - 3) } else { String::new() };
            Tree::Kept { path, why: format!("it holds more than target/: {shown}{more}") }
        }
        Some(_) => {
            let _ = git(repo, &["worktree", "remove", "--force", &dir.display().to_string()]);
            let _ = std::fs::remove_dir_all(dir);
            let _ = git(repo, &["worktree", "prune"]);
            // Checked above, so `-D` loses no commit: every one is on the row's.
            let kept = git(repo, &["branch", "-D", name]).is_none().then(|| name.to_string()).filter(|_| exists(repo, name));
            Tree::Removed { path, kept }
        }
    }
}

/// A leg's tree, if its row and leg are known and the leg is over.
fn settled_tree(store: &Store, repo: &Path, name: &str) -> Option<(PathBuf, PathBuf)> {
    let dir = repo.join(WORKTREES).join(name);
    if !dir.join(".git").exists() {
        return None;
    }
    let (task, leg) = named(store, name)?;
    if !task.status().is_open() || !matches!(leg.state, State::Done | State::Skipped) {
        return None;
    }
    let trunk = cmd_checkout::trunk(repo)?;
    let row_dir = cmd_checkout::checkout_dir(&trunk, &task.id);
    row_dir.join(".git").exists().then_some((dir, row_dir))
}

/// `despawn`'s half: the trees of the legs of `task` that are over, taken on the
/// rule. Trees whose leg is still running or pending are not mentioned.
pub(crate) fn discard(store: &Store, task: &str, standing: &dyn Fn(&Path) -> Option<String>) -> Vec<Tree> {
    let prefix = format!("{task}.");
    let mut out = Vec::new();
    for repo in crate::check_tree::repos(store) {
        for name in leg_trees(&repo, Some(&prefix)) {
            if let Some((dir, row_dir)) = settled_tree(store, &repo, &name) {
                out.push(take(&repo, &dir, &name, &row_dir, standing));
            }
        }
    }
    out
}

/// Every leg tree on disk whose leg is over, taken on the rule.
pub(crate) fn sweep(store: &Store, standing: &dyn Fn(&Path) -> Option<String>) -> Vec<Tree> {
    let mut out = Vec::new();
    for repo in crate::check_tree::repos(store) {
        for name in leg_trees(&repo, None) {
            if let Some((dir, row_dir)) = settled_tree(store, &repo, &name) {
                out.push(take(&repo, &dir, &name, &row_dir, standing));
            }
        }
    }
    out
}

/// The directories under `repo`'s `.worktrees/` that are named like a leg's
/// tree (`<row>.<leg>`), sorted, optionally only those of one row.
fn leg_trees(repo: &Path, prefix: Option<&str>) -> Vec<String> {
    let Ok(entries) = std::fs::read_dir(repo.join(WORKTREES)) else { return Vec::new() };
    let mut names: Vec<String> = entries
        .flatten()
        .map(|e| e.file_name().to_string_lossy().into_owned())
        .filter(|n| n.contains('.') && prefix.is_none_or(|p| n.starts_with(p)))
        .collect();
    names.sort();
    names
}

static SAID: Mutex<BTreeSet<String>> = Mutex::new(BTreeSet::new());

/// The tick's half: [`sweep`] with the live panes as `standing`, and what it did
/// in `cycle.log`. `ended` are seats this step has just ended.
///
/// Not under test, for `check_tree::tidy`'s reason; the tests drive [`sweep`].
pub(crate) fn tidy(store: &Store, ended: &[String]) {
    if cfg!(test) {
        return;
    }
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
    for tree in sweep(store, &standing) {
        say(store, tree);
    }
}

/// One outcome in `cycle.log`; a kept tree once per reason per process.
pub(crate) fn say(store: &Store, tree: Tree) {
    match tree {
        Tree::Removed { path, kept } => {
            let branch = kept.map(|b| format!(" (branch {b} kept)")).unwrap_or_default();
            crate::cycle::log_line(store, &format!("removed leg tree {path}: its leg is over and merged{branch}"));
        }
        Tree::Kept { path, why } => {
            let line = format!("kept leg tree {path}: {why}");
            if SAID.lock().map(|mut s| s.insert(line.clone())).unwrap_or(true) {
                crate::cycle::log_line(store, &line);
            }
        }
        Tree::Absent => {}
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::Project;
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

    fn commit(dir: &Path, file: &str, text: &str) {
        std::fs::write(dir.join(file), text).unwrap();
        run(dir, &["add", file]);
        run(dir, &["commit", "--quiet", "-m", &format!("write {file}")]);
    }

    /// A scratch repository with a project rooted at it, and a row `p-001` with
    /// its tree, whose record has a plan → (a, b) → fin graph.
    fn scratch(name: &str) -> (util::Isolated, Store, PathBuf, PathBuf) {
        let env = util::isolated(&format!("leg-tree-{name}"));
        let repo = env.path("repo");
        std::fs::create_dir_all(&repo).unwrap();
        run(&repo, &["init", "--quiet", "-b", "master"]);
        std::fs::write(repo.join(".gitignore"), "target/\n.worktrees/\n").unwrap();
        std::fs::write(repo.join("shared.txt"), "one\ntwo\nthree\nfour\nfive\n").unwrap();
        run(&repo, &["add", "."]);
        run(&repo, &["commit", "--quiet", "-m", "first"]);
        let store = Store::open();
        let mut p = Project::new("p");
        p.roots = vec![repo.display().to_string()];
        store.save_project(&p).unwrap();
        let row = repo.join(WORKTREES).join("p-001");
        run(&repo, &["worktree", "add", "--quiet", "-b", "p-001", &row.display().to_string(), "master"]);
        let mut t = crate::model::Task::new("a row", "p-001");
        t.project = Some("p".into());
        store.save_task(&t).unwrap();
        (env, store, repo, row)
    }

    fn leg(name: &str, state: State, after: Option<&[&str]>) -> Leg {
        let mut l = Leg::pending(name, "do it");
        l.state = state;
        l.after = after.map(|a| a.iter().map(|s| s.to_string()).collect());
        l
    }

    /// plan → a, b → fin. `a` and `b` are siblings; `plan` and `fin` are not.
    fn graph() -> Vec<Leg> {
        vec![
            leg("plan", State::Done, Some(&[])),
            leg("a", State::Done, Some(&["plan"])),
            leg("b", State::Done, Some(&["plan"])),
            leg("fin", State::Pending, Some(&["a", "b"])),
        ]
    }

    fn record_legs_into(store: &Store, legs: &[Leg]) {
        let mut t = store.task("p-001").unwrap();
        legs::write(&mut t, legs);
        store.save_task(&t).unwrap();
    }

    fn nobody(_: &Path) -> Option<String> {
        None
    }

    fn root(repo: &Path) -> String {
        repo.display().to_string()
    }

    /// Open `a` and `b`, commit in each, and put the trees on the entries.
    fn opened(store: &Store, repo: &Path, files: [(&str, &str); 2]) -> (Vec<Leg>, PathBuf, PathBuf) {
        let mut legs = graph();
        let mut dirs = Vec::new();
        for (name, (file, text)) in ["a", "b"].into_iter().zip(files) {
            let o = open(&root(repo), "p-001", name).unwrap();
            commit(&o.dir, file, text);
            legs.iter_mut().find(|l| l.name == name).unwrap().tree = Some(o.name);
            dirs.push(o.dir);
        }
        record_legs_into(store, &legs);
        (legs, dirs.remove(0), dirs.remove(0))
    }

    #[test]
    fn siblings_are_read_off_the_graph_by_any_path() {
        let legs = graph();
        assert_eq!(siblings(&legs, "a"), vec!["b".to_string()]);
        assert!(has_siblings(&legs, "b"));
        assert!(!has_siblings(&legs, "plan") && !has_siblings(&legs, "fin"));
        // A chain has none, and the d1 record with no `after:` reads as one.
        let chain = vec![leg("x", State::Done, None), leg("y", State::Pending, None), leg("z", State::Pending, None)];
        assert!(chain.iter().all(|l| !has_siblings(&chain, &l.name)));
        // Beside a long way round: c waits on a only through d, still not b's sibling's sibling.
        let long = vec![
            leg("a", State::Done, Some(&[])),
            leg("b", State::Pending, Some(&["a"])),
            leg("c", State::Pending, Some(&["b"])),
            leg("d", State::Pending, Some(&["a"])),
            leg("e", State::Pending, Some(&["c", "d"])),
        ];
        assert_eq!(siblings(&long, "d"), vec!["b".to_string(), "c".to_string()]);
        assert!(!has_siblings(&long, "e"));
    }

    #[test]
    fn two_siblings_each_get_a_tree_off_the_rows_tip() {
        let (_env, store, repo, row) = scratch("open");
        commit(&row, "plan.txt", "the plan\n");
        let tip = git(&row, &["rev-parse", "HEAD"]).unwrap();
        let a = open(&root(&repo), "p-001", "a").unwrap();
        let b = open(&root(&repo), "p-001", "b").unwrap();
        assert!(a.path.ends_with(".worktrees/p-001.a") && b.path.ends_with(".worktrees/p-001.b"), "{} {}", a.path, b.path);
        assert_eq!((a.name.as_str(), b.name.as_str()), ("p-001.a", "p-001.b"));
        for o in [&a, &b] {
            assert_eq!(git(&o.dir, &["rev-parse", "HEAD"]).unwrap(), tip, "{} did not start at the row's tip", o.name);
            assert_eq!(cmd_checkout::trunk_branch(&o.dir).as_deref(), Some(o.name.as_str()));
            assert!(o.dir.join("plan.txt").exists());
        }
        // The row's own tree and branch are untouched, and it is not a stray task's.
        assert_eq!(git(&row, &["rev-parse", "HEAD"]).unwrap(), tip);
        let again = open(&root(&repo), "p-001", "a").unwrap();
        assert_eq!(again.dir, a.dir, "opening again made another tree");
        drop(store);
    }

    #[test]
    fn a_row_with_no_tree_has_nothing_to_cut_from() {
        let (_env, _store, repo, row) = scratch("notree");
        run(&repo, &["worktree", "remove", "--force", &row.display().to_string()]);
        let err = open(&root(&repo), "p-001", "a").unwrap_err();
        assert!(err.contains("has no tree"), "{err}");
    }

    #[test]
    fn a_clean_join_merges_both_and_the_trees_go() {
        let (_env, store, repo, row) = scratch("clean");
        let (mut legs, a, b) = opened(&store, &repo, [("a.txt", "from a\n"), ("b.txt", "from b\n")]);

        let joined = join(&row, &legs, "fin");
        assert!(joined.clean(), "{joined:?}");
        assert_eq!(joined.merged.iter().map(|m| m.leg.as_str()).collect::<Vec<_>>(), ["a", "b"]);
        assert!(row.join("a.txt").exists() && row.join("b.txt").exists(), "the row's tree lacks a leg's work");
        assert_eq!(cmd_checkout::uncommitted(&row), 0);
        let brief = joined.brief().unwrap();
        assert!(brief.contains("merged a (p-001.a)") && brief.contains("merged b (p-001.b)"), "{brief}");

        // A second call finds nothing to do.
        assert_eq!(join(&row, &legs, "fin"), Joined::default());

        // Both legs are over: their trees go, branches with them, and a second sweep finds nothing.
        for l in legs.iter_mut() {
            l.ending = crate::verification::Ending::Ended;
        }
        record_legs_into(&store, &legs);
        let out = sweep(&store, &nobody);
        assert_eq!(out.len(), 2, "{}", out.len());
        assert!(out.iter().all(|t| matches!(t, Tree::Removed { kept: None, .. })));
        assert!(!a.exists() && !b.exists());
        assert!(git(&repo, &["rev-parse", "--verify", "--quiet", "refs/heads/p-001.a"]).is_none(), "the branch stayed");
        assert!(sweep(&store, &nobody).is_empty());
        assert!(row.exists(), "the row's own tree went");
    }

    #[test]
    fn a_conflicting_pair_leaves_the_rows_tree_clean_at_a_commit_and_names_the_branch_and_files() {
        let (_env, store, repo, row) = scratch("conflict");
        let (legs, a, b) = opened(&store, &repo, [("shared.txt", "one\nA\nthree\nfour\nfive\n"), ("shared.txt", "one\nB\nthree\nfour\nfive\n")]);
        // And a third branch behind the conflict, to show it is handed on, not tried.
        let before = git(&row, &["rev-parse", "HEAD"]).unwrap();
        let mut three = legs.clone();
        let c = open(&root(&repo), "p-001", "c").unwrap();
        commit(&c.dir, "c.txt", "c\n");
        three.push(leg("c", State::Done, Some(&["plan"])));
        three.iter_mut().find(|l| l.name == "c").unwrap().tree = Some(c.name);
        three.iter_mut().find(|l| l.name == "fin").unwrap().after = Some(vec!["a".into(), "b".into(), "c".into()]);

        let joined = join(&row, &three, "fin");
        // a merged (it went first), b conflicted, c was not tried.
        assert_eq!(joined.merged.iter().map(|m| m.leg.as_str()).collect::<Vec<_>>(), ["a"]);
        let conflict = joined.conflict.as_ref().expect("no conflict reported");
        assert_eq!((conflict.leg.as_str(), conflict.branch.as_str()), ("b", "p-001.b"));
        assert_eq!(conflict.files, vec!["shared.txt".to_string()]);
        assert_eq!(joined.waiting, vec![("c".to_string(), "p-001.c".to_string())]);
        assert_ne!(git(&row, &["rev-parse", "HEAD"]).unwrap(), before, "a's merge should be kept");
        assert_eq!(cmd_checkout::uncommitted(&row), 0, "the row's tree is not clean");
        assert!(!row.join(".git").is_dir() || !row.join(".git/MERGE_HEAD").exists());
        let merge_head = git(&row, &["rev-parse", "--verify", "--quiet", "MERGE_HEAD"]);
        assert!(merge_head.is_none(), "a merge was left open");
        assert!(!joined.clean());
        let brief = joined.brief().unwrap();
        assert!(brief.contains("CONFLICT merging b (p-001.b)") && brief.contains("shared.txt") && brief.contains("c (p-001.c)"), "{brief}");

        // The unmerged legs' trees are kept, with the reason.
        let mut over = three.clone();
        for l in over.iter_mut() {
            l.state = State::Done;
        }
        record_legs_into(&store, &over);
        let kept: Vec<_> = sweep(&store, &nobody);
        let blocked: Vec<&Tree> = kept.iter().filter(|t| matches!(t, Tree::Kept { .. })).collect();
        assert_eq!(blocked.len(), 2, "b and c are not merged");
        assert!(b.exists());
        assert!(!a.exists(), "a was merged and over, so its tree goes");
    }

    #[test]
    fn edits_in_different_places_of_one_file_merge_without_anyone_being_told() {
        let (_env, store, repo, row) = scratch("places");
        let (legs, _a, _b) = opened(&store, &repo, [("shared.txt", "ONE\ntwo\nthree\nfour\nfive\n"), ("shared.txt", "one\ntwo\nthree\nfour\nFIVE\n")]);
        // `b` was cut before `a` merged, so its branch also differs from the row's by this file.
        let joined = join(&row, &legs, "fin");
        assert!(joined.clean(), "{joined:?}");
        assert_eq!(std::fs::read_to_string(row.join("shared.txt")).unwrap(), "ONE\ntwo\nthree\nfour\nFIVE\n");
    }

    #[test]
    fn a_join_holds_while_someone_stands_in_the_rows_tree_or_it_is_dirty() {
        let (_env, store, repo, row) = scratch("held");
        let (mut legs, _a, _b) = opened(&store, &repo, [("a.txt", "a\n"), ("b.txt", "b\n")]);
        // A running leg with no tree of its own is standing in the row's.
        legs.push(leg("side", State::Running, Some(&["plan"])));
        let joined = join(&row, &legs, "fin");
        assert!(joined.merged.is_empty() && joined.held.as_deref().is_some_and(|h| h.contains("side")), "{joined:?}");
        assert_eq!(joined.waiting.len(), 2);
        assert!(!row.join("a.txt").exists());

        legs.pop();
        std::fs::write(row.join("shared.txt"), "edited\n").unwrap();
        let joined = join(&row, &legs, "fin");
        assert!(joined.held.as_deref().is_some_and(|h| h.contains("uncommitted")), "{joined:?}");
        assert!(joined.brief().unwrap().contains("nothing was merged"));

        run(&row, &["checkout", "--", "shared.txt"]);
        assert!(join(&row, &legs, "fin").clean());
    }

    #[test]
    fn a_tree_with_an_untracked_file_is_kept_and_named() {
        let (_env, store, repo, row) = scratch("litter");
        let (legs, a, b) = opened(&store, &repo, [("a.txt", "a\n"), ("b.txt", "b\n")]);
        assert!(join(&row, &legs, "fin").clean());
        std::fs::write(a.join("notes.txt"), "a leg left this\n").unwrap();
        std::fs::create_dir_all(b.join("target/debug")).unwrap();
        std::fs::write(b.join("target/debug/big"), "build output\n").unwrap();

        let out = sweep(&store, &nobody);
        let kept: Vec<&Tree> = out.iter().filter(|t| matches!(t, Tree::Kept { .. })).collect();
        match kept.as_slice() {
            [Tree::Kept { path, why }] => {
                assert!(path.ends_with("p-001.a") && why.contains("notes.txt"), "{path}: {why}");
            }
            _ => panic!("the tree holding a file was not kept"),
        }
        assert!(a.exists() && !b.exists(), "b has only a target/ and goes");
        // Named a second time too, by despawn's half.
        assert!(matches!(discard(&store, "p-001", &nobody).as_slice(), [Tree::Kept { .. }]));
    }

    #[test]
    fn a_pane_standing_in_it_or_a_leg_still_going_keeps_the_tree() {
        let (_env, store, repo, row) = scratch("standing");
        let (mut legs, a, _b) = opened(&store, &repo, [("a.txt", "a\n"), ("b.txt", "b\n")]);
        assert!(join(&row, &legs, "fin").clean());
        let here = |d: &Path| (util::real(&d.display().to_string()) == util::real(&a.display().to_string())).then(|| "cpd-7 is standing in it".to_string());
        let out = sweep(&store, &here);
        assert!(out.iter().any(|t| matches!(t, Tree::Kept { why, .. } if why.contains("cpd-7"))));
        assert!(a.exists());

        legs.iter_mut().find(|l| l.name == "a").unwrap().state = State::Running;
        record_legs_into(&store, &legs);
        sweep(&store, &nobody);
        assert!(a.exists(), "a running leg's tree was swept");

        // A closed row's trees are the row's to take, not this sweep's.
        legs.iter_mut().find(|l| l.name == "a").unwrap().state = State::Done;
        record_legs_into(&store, &legs);
        let mut t = store.task("p-001").unwrap();
        t.set_status(crate::model::Status::Done);
        store.save_task(&t).unwrap();
        assert!(sweep(&store, &nobody).is_empty());
        assert!(a.exists());
    }

    #[test]
    fn only_a_legs_tree_is_swept() {
        let (_env, store, repo, row) = scratch("only");
        let stranger = repo.join(WORKTREES).join("nolist.leg");
        run(&repo, &["worktree", "add", "--quiet", "-b", "nolist.leg", &stranger.display().to_string(), "master"]);
        let unknown_leg = repo.join(WORKTREES).join("p-001.nothing");
        run(&repo, &["worktree", "add", "--quiet", "-b", "p-001.nothing", &unknown_leg.display().to_string(), "master"]);
        record_legs_into(&store, &graph());
        assert!(sweep(&store, &nobody).is_empty());
        assert!(stranger.exists() && unknown_leg.exists() && row.exists());
    }

    #[test]
    fn a_sibling_hears_which_paths_another_has_also_changed() {
        let (_env, store, repo, row) = scratch("overlap");
        let (legs, _a, _b) = opened(&store, &repo, [("shared.txt", "A\n"), ("shared.txt", "B\n")]);
        let found = overlaps(&row, &legs, "a");
        assert_eq!(found, vec![("b".to_string(), vec!["shared.txt".to_string()])]);
        let line = overlap_line("a", &found).unwrap();
        assert!(line.contains("b: shared.txt"), "{line}");

        // Files apart: nothing to say. (One scratch repository at a time: the
        // isolated environment is held for the test's life.)
        run(&legs[2].tree.as_ref().map(|t| repo.join(WORKTREES).join(t)).unwrap(), &["reset", "--hard", "--quiet", "HEAD~1"]);
        commit(&repo.join(WORKTREES).join("p-001.b"), "b.txt", "B\n");
        assert!(overlaps(&row, &legs, "a").is_empty());
        assert_eq!(overlap_line("a", &[]), None);
        drop(store);
    }

    #[test]
    fn a_leg_tree_reads_as_its_rows_to_checkout() {
        assert_eq!(row_of("wsp-287.build-store"), "wsp-287");
        assert_eq!(row_of("wsp-287"), "wsp-287");

        let (_env, store, repo, row) = scratch("checkout");
        let (legs, a, _b) = opened(&store, &repo, [("a.txt", "a\n"), ("b.txt", "b\n")]);
        assert!(join(&row, &legs, "fin").clean());

        // `finished` and `naming` give the row's answer for a leg's name.
        let closed = cmd_checkout::finished(&store);
        assert!(!closed("p-001.a"));
        let whose = cmd_checkout::naming(&store);
        assert_eq!(whose("p-001.a"), cmd_checkout::Whose::Task);
        assert_eq!(whose("p-001.zzz"), cmd_checkout::Whose::Task, "a name of the row's is the row's");
        assert_eq!(whose("q-009.a"), cmd_checkout::Whose::Stranger);

        // A leg's branch with no tree and work on it is a stray *of the row*, named as a task's.
        run(&repo, &["worktree", "remove", "--force", &a.display().to_string()]);
        let strays = cmd_checkout::strays(&repo, &whose);
        assert!(strays.iter().any(|s| s.branch == "p-001.a" && s.whose == cmd_checkout::Whose::Task), "{}", strays.len());

        // And once the row is closed, its trees are stale and swept with it.
        let b_dir = repo.join(WORKTREES).join("p-001.b");
        let mut t = store.task("p-001").unwrap();
        t.set_status(crate::model::Status::Done);
        store.save_task(&t).unwrap();
        let closed = cmd_checkout::finished(&store);
        let found = cmd_checkout::stale(&repo, &closed, &|_| false);
        let names: Vec<&str> = found.iter().map(|s| s.task.as_str()).collect();
        assert!(names.contains(&"p-001.b"), "{names:?}");
        assert!(b_dir.exists());
    }

    /// What the spawn does: a sibling is cut a tree off the row's tip, a join
    /// merges first and carries the account on its entry, and a chain leg is
    /// left in the row's tree.
    #[test]
    fn the_spawns_prepare_cuts_siblings_trees_and_merges_at_the_join() {
        let (_env, store, repo, row) = scratch("prepare");
        let row_cwd = util::contract(&row);
        let mut legs = graph();
        for n in ["a", "b"] {
            legs.iter_mut().find(|l| l.name == n).unwrap().state = State::Pending;
        }
        record_legs(&store, &legs);

        let a = prepare(&store, &root(&repo), &row_cwd, "p-001", "a").unwrap();
        let b = prepare(&store, &root(&repo), &row_cwd, "p-001", "b").unwrap();
        assert_eq!((a.tree.as_deref(), b.tree.as_deref()), (Some("p-001.a"), Some("p-001.b")));
        assert!(a.cwd.as_deref().unwrap().ends_with(".worktrees/p-001.a"), "{:?}", a.cwd);
        assert_eq!(a.join, None);
        // The plan has no siblings and stands in the row's tree.
        assert_eq!(prepare(&store, &root(&repo), &row_cwd, "p-001", "plan").unwrap(), Prepared::default());
        // A leg that is not ready is left for `open` to refuse, with nothing made.
        assert_eq!(prepare(&store, &root(&repo), &row_cwd, "p-001", "fin").unwrap(), Prepared::default());
        assert!(!repo.join(WORKTREES).join("p-001.fin").exists());

        // Both legs work and finish; their trees are on their entries.
        let mut done = graph();
        for (n, pr, file) in [("a", &a, "shared.txt"), ("b", &b, "shared.txt")] {
            let dir = repo.join(WORKTREES).join(pr.tree.as_deref().unwrap());
            commit(&dir, file, &format!("{n}\n"));
            done.iter_mut().find(|l| l.name == n).unwrap().tree = pr.tree.clone();
        }
        record_legs(&store, &done);
        let fin = prepare(&store, &root(&repo), &row_cwd, "p-001", "fin").unwrap();
        assert_eq!(fin.cwd, None, "the last leg stands in the row's tree");
        let text = fin.join.clone().unwrap();
        assert!(text.contains("merged a") && text.contains("CONFLICT merging b") && text.contains("shared.txt"), "{text}");
        assert_eq!(cmd_checkout::uncommitted(&row), 0);

        // It rides on the entry, through the record's own text, into the brief.
        record(&store, "p-001", "fin", &fin);
        let t = store.task("p-001").unwrap();
        let back = legs::legs(&t);
        assert_eq!(back.iter().find(|l| l.name == "fin").unwrap().join.as_deref(), Some(text.as_str()));
        let mut running = back.clone();
        let f = running.iter_mut().find(|l| l.name == "fin").unwrap();
        f.state = State::Running;
        f.pane = Some("cpd-9".into());
        let mut t = t;
        legs::write(&mut t, &running);
        let lines = crate::leg_seat::brief_lines(&[t], "cpd-9").unwrap().join("\n");
        assert!(lines.contains("the join:") && lines.contains("CONFLICT merging b"), "{lines}");
    }

    /// The spawn falls back to the project root when the row has no tree. A
    /// join must then be refused with nothing merged: master is where that
    /// merge would land.
    #[test]
    fn a_join_is_refused_when_the_row_has_no_tree_and_master_does_not_move() {
        let (_env, store, repo, _row) = scratch("no-row-tree");
        let mut done = graph();
        for n in ["a", "b"] {
            let name = tree_name("p-001", n);
            let dir = repo.join(WORKTREES).join(&name);
            run(&repo, &["worktree", "add", "--quiet", "-b", &name, &dir.display().to_string(), "p-001"]);
            commit(&dir, &format!("{n}.txt"), "work\n");
            done.iter_mut().find(|l| l.name == n).unwrap().tree = Some(name);
        }
        record_legs(&store, &done);
        let master = git(&repo, &["rev-parse", "master"]).unwrap();

        let err = prepare(&store, &root(&repo), &root(&repo), "p-001", "fin").unwrap_err();
        assert!(err.contains("no tree of its own"), "{err}");
        assert_eq!(git(&repo, &["rev-parse", "master"]).unwrap(), master, "nothing merged into master");
        assert_eq!(git(&repo, &["rev-parse", "HEAD"]).unwrap(), master);
        assert_eq!(cmd_checkout::uncommitted(&repo), 0);

        // The same call from the row's own tree goes through.
        let row = repo.join(WORKTREES).join("p-001");
        let ok = prepare(&store, &root(&repo), &util::contract(&row), "p-001", "fin").unwrap();
        assert!(ok.join.unwrap().contains("merged a"));
    }

    fn record_legs(store: &Store, legs: &[Leg]) {
        record_legs_into(store, legs);
    }
}
