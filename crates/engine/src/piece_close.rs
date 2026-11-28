//! A batch's close, as a function: `docs/pieces.md` *Batches and trees*, wsp-505.
//!
//! [`close`] is what the run does when the last piece of a batch ends. It has no
//! seat and writes nothing but the one piece it may add, and it reuses
//! [`crate::leg_tree`] for the git half:
//!
//! 1. **Merge** each done piece's branch into the row's branch, in the row's
//!    tree, in piece order, by `leg_tree::join`'s rules (the pieces are handed to
//!    `join` as the legs it waits on, so there is one merge and one set of rules).
//! 2. **Check** the batch's `check:` lines on the merged tip, in the row's tree,
//!    with greens reused by input hash as `wsp check --affected --run` does.
//! 3. **Clean and green**: the pieces' trees go by `check_tree`'s rule
//!    (`leg_tree::take`), and what a tree holds beyond build output is kept and
//!    named.
//! 4. **A conflict or a red step** adds one piece in a new batch right after this
//!    one ([`pieces::insert_batch_after`]): `merge batch N` or `fix batch N: <label>`.
//!
//! The run calls [`close`] when a batch's last piece ends
//! (`cycle::close_batches`, wsp-506). The tests drive [`close_with`].
//!
//! # The choices the doc left open (wsp-505)
//!
//! - **A batch of one** has no tree, so there is nothing to merge; its checks run
//!   on the row's tip and are answered by the ledger where its own `piece done`
//!   has already run them there.
//! - **Checks are the done pieces' `check:` lines**, labelled `<n>.<label>`, and a
//!   command two pieces both carry runs once (under the first one's label).
//! - **Closing twice is safe.** A branch already in the row's is passed over, and
//!   a `merge batch N` or `fix batch N: …` piece already filed is returned
//!   again rather than filed a second time.
//! - **Held is not an answer.** Someone standing in the row's tree, or
//!   uncommitted changes in it, merges nothing and says so; the caller tries again.

use std::path::{Path, PathBuf};

use crate::check::{self, Of, Step};
use crate::check_tree::Residue;
use crate::cmd_checkout::{self, Tree, WORKTREES};
use crate::cmd_verify::git;
use crate::greens::{self, Reuse};
use crate::legs::{self, Leg};
use crate::pieces::{self, New, Piece, State};
use crate::store::Store;
use crate::util;

/// What closing a batch came to.
#[derive(Debug, PartialEq, Eq)]
pub enum Closed {
    /// Not a batch that can close: no such batch, or a piece in it still to end.
    Open(String),
    /// Nothing was merged or run, and why. The caller tries again.
    Held(String),
    /// Merged and green. `merged` are the pieces whose branches this call
    /// merged; `trees` the trees that went and the ones kept, with why.
    Clean { merged: Vec<usize>, ran: usize, reused: usize, removed: Vec<String>, kept: Vec<(String, String)> },
    /// A branch did not merge, and was aborted. `added` is the `merge batch N`
    /// piece, in the batch after this one.
    Conflict { branches: Vec<String>, files: Vec<String>, added: usize },
    /// The merged tip failed `step`, which is `piece`'s line. `added` is the
    /// `fix batch N: <label>` piece, in the batch after this one.
    Red { step: String, piece: usize, added: usize },
}

/// [`close_with`], where "standing" is a live agent's working directory. The run's
/// tests answer for it (`cycle::tests::CLOSE_AS`), so only the binary calls it.
#[cfg_attr(test, allow(dead_code))]
pub fn close(store: &Store, row: &str, batch: usize) -> Closed {
    let seen = std::cell::OnceCell::new();
    let standing = |dir: &Path| -> Option<String> {
        let dir = util::real(&dir.display().to_string());
        match seen.get_or_init(cmd_checkout::who_is_standing) {
            Ok(rows) => rows.iter().find(|(_, cwd)| util::real(cwd).starts_with(&dir)).map(|(id, _)| format!("{id} is standing in it")),
            Err(why) => Some(why.clone()),
        }
    };
    close_with(store, row, batch, &standing)
}

/// Close batch `batch` of `row`. `standing` says who is in a directory.
pub fn close_with(store: &Store, row: &str, batch: usize, standing: &dyn Fn(&Path) -> Option<String>) -> Closed {
    let Some(t) = store.task(row) else { return Closed::Open(format!("no row {row}")) };
    let rec = pieces::record(&t);
    let mut members: Vec<&Piece> = rec.pieces.iter().filter(|p| p.batch == batch).collect();
    members.sort_by_key(|p| p.n);
    if members.is_empty() {
        return Closed::Open(format!("{row} has no piece in batch {batch}"));
    }
    if let Some(p) = members.iter().find(|p| !matches!(p.state, State::Done | State::Dropped | State::Promoted)) {
        return Closed::Open(format!("piece {} is {}, and a batch closes when every piece in it is done, dropped or promoted", p.n, p.state.as_str()));
    }
    let Some((repo, row_dir)) = row_tree(store, row) else {
        return Closed::Held(format!("{row} has no tree of its own to merge in"));
    };
    let done: Vec<&Piece> = members.iter().copied().filter(|p| p.state == State::Done).collect();
    // What runs the batch's checks, chosen before anything is merged: a
    // manager that cannot be reached holds the batch with nothing done.
    let runner = match check::Runner::chosen(store, check::Whose::Row(row), check::TreeKind::Work, &row_dir, None) {
        Ok(r) => r,
        Err(why) => return Closed::Held(why),
    };

    // ---- merge
    let branched: Vec<&Piece> = done.iter().copied().filter(|p| p.tree.is_some()).collect();
    let mut merged = Vec::new();
    if !branched.is_empty() {
        if let Some(why) = standing(&row_dir) {
            return Closed::Held(format!("nothing was merged: {why}"));
        }
        let mut graph: Vec<Leg> = branched
            .iter()
            .map(|p| {
                let mut l = Leg::pending(&p.id(row), "");
                l.state = legs::State::Done;
                l.tree = p.tree.clone();
                l.after = Some(Vec::new());
                l
            })
            .collect();
        let mut close_leg = Leg::pending("close", "");
        close_leg.after = Some(graph.iter().map(|l| l.name.clone()).collect());
        graph.push(close_leg);
        let joined = crate::leg_tree::join(&row_dir, &graph, "close");
        if let Some(why) = joined.held {
            return Closed::Held(format!("nothing was merged: {why}"));
        }
        let number = |leg: &str| branched.iter().find(|p| p.id(row) == leg).map(|p| p.n).unwrap_or(0);
        merged = joined.merged.iter().map(|m| number(&m.leg)).collect();
        if let Some(c) = joined.conflict {
            let mut branches = vec![c.branch];
            branches.extend(joined.waiting.into_iter().map(|(_, b)| b));
            let overview = conflict_overview(batch, &merged_branches(&joined.merged), &branches, &c.files);
            let new = piece_for(format!("merge batch {batch}"), overview, &batch_checks(row, &done), "git merge --no-edit left the row's tree clean at a commit");
            let added = file(store, row, batch, new);
            return Closed::Conflict { branches, files: c.files, added: added.unwrap_or(0) };
        }
    }

    // ---- check
    let wanted = batch_checks(row, &done);
    let steps: Vec<Step> = wanted.iter().map(|(_, s, _)| s.clone()).collect();
    let (mut ran, mut reused) = (0, 0);
    if !steps.is_empty() {
        let head = git(&row_dir, &["rev-parse", "HEAD"]).map(|h| h.trim().to_string()).unwrap_or_default();
        let ledger = t.project.as_deref().map(|p| greens::Greens::open(store, p, &repo));
        let reuse = ledger.as_ref().map(|g| Reuse { greens: g, sha: &head, env: greens::Env::Warm, who: row });
        let dir = store.state_file("checks");
        let _ = std::fs::create_dir_all(&dir);
        let results = check::run_row(store, &t, &row_dir, &steps, &dir.join(format!("{row}-batch-{batch}.log")), reuse.as_ref(), &runner, |_| {});
        let (r, n) = check::tally(&results);
        (reused, ran) = (r, n);
        // `wsp-067`: a tree that went under the run is no red to file a fix for.
        if let Some(gone) = results.iter().find(|r| matches!(r.done, check::Done::TreeMissing(_))) {
            return Closed::Held(check::why_failed(gone));
        }
        if let Some(bad) = results.iter().find(|r| r.done.is_red()) {
            let (piece, label, line) = wanted
                .iter()
                .find(|(_, s, _)| s.name == bad.step.name)
                .map(|(n, s, line)| (*n, s.name.clone(), line.clone()))
                .unwrap_or((0, bad.step.name.clone(), String::new()));
            let label = label.split_once('.').map(|(_, l)| l.to_string()).unwrap_or(label);
            let overview = red_overview(batch, &label, piece, &line, &head, &check::why_failed(bad), &bad.tail);
            let new = piece_for(format!("fix batch {batch}: {label}"), overview, &wanted, "");
            let added = file(store, row, batch, new);
            return Closed::Red { step: label, piece, added: added.unwrap_or(0) };
        }
    }

    // ---- clean and green: the trees go
    let residue = Residue::of(store, &repo);
    let (mut removed, mut kept) = (Vec::new(), Vec::new());
    for name in branched.iter().filter_map(|p| p.tree.as_deref()) {
        let dir = repo.join(WORKTREES).join(name);
        if !dir.join(".git").exists() {
            continue;
        }
        match crate::leg_tree::take(&repo, &dir, name, &row_dir, &residue, standing) {
            Tree::Removed { path, .. } => removed.push(path),
            Tree::Kept { path, why } => kept.push((path, why)),
            Tree::Absent => {}
        }
    }
    Closed::Clean { merged, ran, reused, removed, kept }
}

/// The row's repository and its tree, if it has one.
fn row_tree(store: &Store, row: &str) -> Option<(PathBuf, PathBuf)> {
    crate::check_tree::repos(store).into_iter().find_map(|repo| {
        let trunk = cmd_checkout::trunk(&repo)?;
        let dir = cmd_checkout::checkout_dir(&trunk, row);
        dir.join(".git").exists().then_some((repo, dir))
    })
}

fn merged_branches(m: &[crate::leg_tree::Merge]) -> Vec<String> {
    m.iter().map(|m| m.branch.clone()).collect()
}

/// The batch's check lines on the done pieces, in piece order and each command
/// once: `(piece, step labelled <n>.<label>, the line as written)`.
fn batch_checks(_row: &str, done: &[&Piece]) -> Vec<(usize, Step, String)> {
    let mut seen = std::collections::BTreeSet::new();
    let mut out = Vec::new();
    for p in done {
        for line in &p.checks {
            let Some(mut step) = check::step_of(line, Of::Task) else { continue };
            if !seen.insert(step.command.clone()) {
                continue;
            }
            step.name = format!("{}.{}", p.n, step.name);
            out.push((p.n, step, line.clone()));
        }
    }
    out
}

fn conflict_overview(batch: usize, merged: &[String], branches: &[String], files: &[String]) -> String {
    let files = match files.is_empty() {
        true => "files git did not name".to_string(),
        false => files.join(", "),
    };
    let first = match merged.is_empty() {
        true => String::new(),
        false => format!(" Merged already: {}.", merged.join(", ")),
    };
    format!(
        "Closing batch {batch} merged its pieces' branches into the row's branch in piece order, and {} did not merge; the merge was aborted, so the row's tree is clean at a commit.{first} \
         Still to merge, with `git merge --no-edit <branch>` in the row's tree, in this order: {}. Conflicting files: {files}. \
         Resolve them keeping both pieces' intent, commit, and run the batch's checks on the merged tip.",
        branches[0],
        branches.join(", ")
    )
}

fn red_overview(batch: usize, label: &str, piece: usize, line: &str, sha: &str, why: &str, tail: &str) -> String {
    let at: String = sha.chars().take(12).collect();
    let whose = match piece {
        0 => String::new(),
        n => format!(" It is piece {n}'s line: `{line}`."),
    };
    let tail = match tail.trim().is_empty() {
        true => String::new(),
        false => format!(" The end of its output:\n{}", tail.trim()),
    };
    let text = format!(
        "Closing batch {batch} merged its pieces' branches clean, and the check `{label}` is red on the merged tip {at} ({why}).{whose} \
         Find what the merge of these pieces broke and fix it in the row's tree.{tail}"
    );
    util::truncate(&text, pieces::OVERVIEW_CAP)
}

/// The piece a close adds, on the default tier. `checks` are the batch's, so
/// the piece's own `piece done` runs what the batch's close will.
fn piece_for(title: String, overview: String, checks: &[(usize, Step, String)], _why: &str) -> New {
    let labels: Vec<String> = checks.iter().map(|(_, s, _)| s.name.clone()).collect();
    let tail = match labels.is_empty() {
        true => String::new(),
        false => format!(": {}", labels.join(", ")),
    };
    New {
        title: util::truncate(&util::fold(&title), pieces::TITLE_CAP),
        at: pieces::At::Next,
        overview: util::truncate(&overview, pieces::OVERVIEW_CAP),
        done_when: util::truncate(&format!("the batch's checks green on the merged tip{tail}"), pieces::DONE_WHEN_CAP),
        checks: checks.iter().map(|(_, _, line)| line.clone()).collect(),
        agent: None,
        why: None,
    }
}

/// Write `new` as a batch right after `batch`, under the store lock, and its
/// number. A piece of that title already filed after `batch` is returned as it
/// is: closing again does not file a second.
fn file(store: &Store, row: &str, batch: usize, new: New) -> Option<usize> {
    store.locked(|| {
        let mut t = store.task(row)?;
        let rec = pieces::record(&t);
        if let Some(p) = rec.pieces.iter().find(|p| p.batch > batch && p.title == new.title.trim() && p.state != State::Dropped) {
            return Some(p.n);
        }
        let (rec, n) = pieces::insert_batch_after(&rec, batch, new, t.status()).ok()?;
        pieces::write(&mut t, &rec);
        store.save_task(&t).ok()?;
        Some(n)
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::Project;
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

    fn commit(dir: &Path, file: &str, text: &str) {
        std::fs::write(dir.join(file), text).unwrap();
        run(dir, &["add", file]);
        run(dir, &["commit", "--quiet", "-m", &format!("write {file}")]);
    }

    /// A scratch repository with a project rooted at it, and a row `p-001` with its tree.
    fn scratch(name: &str) -> (util::Isolated, Store, PathBuf, PathBuf) {
        let env = util::isolated(&format!("piece-close-{name}"));
        let repo = env.path("repo");
        std::fs::create_dir_all(&repo).unwrap();
        crate::testkit::repo(&repo);
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

    fn nobody(_: &Path) -> Option<String> {
        None
    }

    /// A tree and a branch for piece `n`, cut from the row's tip, with one commit in it.
    fn piece_tree(repo: &Path, n: usize, file: &str, text: &str) -> String {
        let name = format!("p-001-{n}");
        let dir = repo.join(WORKTREES).join(&name);
        run(repo, &["worktree", "add", "--quiet", "-b", &name, &dir.display().to_string(), "p-001"]);
        commit(&dir, file, text);
        name
    }

    fn done(n: usize, batch: usize, tree: Option<&str>, checks: &[&str]) -> Piece {
        let mut p = Piece::pending(n, &format!("piece {n}"), batch);
        p.state = State::Done;
        p.tree = tree.map(str::to_string);
        p.overview = "do it".into();
        p.done_when = "it is done".into();
        p.checks = checks.iter().map(|c| c.to_string()).collect();
        p
    }

    fn put(store: &Store, ps: Vec<Piece>) {
        let mut t = store.task("p-001").unwrap();
        pieces::write(&mut t, &pieces::Record { cap: pieces::DEFAULT_CAP, closed: Vec::new(), pieces: ps });
        store.save_task(&t).unwrap();
    }

    fn record(store: &Store) -> pieces::Record {
        pieces::record(&store.task("p-001").unwrap())
    }

    fn marker_lines(path: &Path) -> Vec<String> {
        let mut v: Vec<String> = std::fs::read_to_string(path).unwrap_or_default().lines().map(str::to_string).collect();
        v.sort();
        v
    }

    #[test]
    fn two_pieces_with_disjoint_edits_merge_clean_the_checks_run_once_and_both_trees_go() {
        let (env, store, repo, row) = scratch("clean");
        let m = env.path("marker");
        let (a, b) = (piece_tree(&repo, 1, "a.txt", "from a\n"), piece_tree(&repo, 2, "b.txt", "from b\n"));
        let say = |what: &str| format!("{what} (1m): echo {what} >> {}", m.display());
        put(&store, vec![done(1, 1, Some(&a), &[&say("one"), &say("shared")]), done(2, 1, Some(&b), &[&say("shared"), &say("two")])]);

        let out = close_with(&store, "p-001", 1, &nobody);
        let Closed::Clean { merged, ran, removed, kept, .. } = out else { panic!("{out:?}") };
        assert_eq!(merged, vec![1, 2]);
        assert_eq!(ran, 3, "the shared command is one step");
        assert!(kept.is_empty(), "{kept:?}");
        assert_eq!(removed.len(), 2);
        assert_eq!(marker_lines(&m), ["one", "shared", "two"], "each check ran once");
        assert!(row.join("a.txt").exists() && row.join("b.txt").exists());
        assert_eq!(cmd_checkout::uncommitted(&row), 0);
        for name in [&a, &b] {
            assert!(!repo.join(WORKTREES).join(name).exists(), "{name}'s tree stayed");
            assert!(git(&repo, &["rev-parse", "--verify", "--quiet", &format!("refs/heads/{name}")]).is_none(), "{name}'s branch stayed");
        }
        assert_eq!(record(&store).pieces.len(), 2, "a clean close adds nothing");
    }

    #[test]
    fn a_conflicting_pair_leaves_the_tree_clean_and_files_a_merge_piece_in_a_new_batch_after() {
        let (_env, store, repo, row) = scratch("conflict");
        let a = piece_tree(&repo, 1, "shared.txt", "one\nA\nthree\nfour\nfive\n");
        let b = piece_tree(&repo, 2, "shared.txt", "one\nB\nthree\nfour\nfive\n");
        let mut later = Piece::pending(3, "later", 2);
        later.overview = "after".into();
        later.done_when = "done".into();
        put(&store, vec![done(1, 1, Some(&a), &["ok (1m): true"]), done(2, 1, Some(&b), &[]), later]);

        let out = close_with(&store, "p-001", 1, &nobody);
        let Closed::Conflict { branches, files, added } = out else { panic!("{out:?}") };
        assert_eq!(branches, vec![b.clone()]);
        assert_eq!(files, vec!["shared.txt".to_string()]);
        assert_eq!(cmd_checkout::uncommitted(&row), 0, "the row's tree is not clean");
        assert!(git(&row, &["rev-parse", "--verify", "--quiet", "MERGE_HEAD"]).is_none(), "a merge was left open");
        assert!(row.join("shared.txt").exists());

        let rec = record(&store);
        let p = rec.pieces.iter().find(|p| p.n == added).unwrap();
        assert_eq!((p.title.as_str(), p.batch, p.state), ("merge batch 1", 2, State::Pending));
        assert!(p.overview.contains(&b) && p.overview.contains("shared.txt"), "{}", p.overview);
        assert!(p.done_when.contains("green on the merged tip"), "{}", p.done_when);
        assert_eq!(rec.pieces.iter().find(|p| p.n == 3).unwrap().batch, 3, "the batch that was next moved up");
        // The trees of the unmerged pieces stay.
        assert!(repo.join(WORKTREES).join(&b).exists());

        // Closing again files nothing more.
        let again = close_with(&store, "p-001", 1, &nobody);
        assert!(matches!(again, Closed::Conflict { added: n, .. } if n == added), "{again:?}");
        assert_eq!(record(&store).pieces.len(), 4);
    }

    #[test]
    fn a_red_step_files_a_fix_piece_naming_the_piece_whose_line_it_is() {
        let (_env, store, repo, _row) = scratch("red");
        let a = piece_tree(&repo, 1, "a.txt", "from a\n");
        let b = piece_tree(&repo, 2, "b.txt", "from b\n");
        put(&store, vec![done(1, 1, Some(&a), &["fine (1m): true"]), done(2, 1, Some(&b), &["broken (1m): exit 3"])]);

        let out = close_with(&store, "p-001", 1, &nobody);
        let Closed::Red { step, piece, added } = out else { panic!("{out:?}") };
        assert_eq!((step.as_str(), piece), ("broken", 2));
        let rec = record(&store);
        let p = rec.pieces.iter().find(|p| p.n == added).unwrap();
        assert_eq!((p.title.as_str(), p.batch), ("fix batch 1: broken", 2));
        assert!(p.overview.contains("piece 2's line") && p.overview.contains("exit 3"), "{}", p.overview);
        assert!(p.done_when.contains("green on the merged tip"));
        // The merge went; the trees wait for the fix.
        assert!(repo.join(WORKTREES).join(&a).exists());
    }

    #[test]
    fn a_batch_of_one_in_the_rows_tree_closes_with_no_merge_and_reruns_nothing() {
        let (env, store, repo, row) = scratch("one");
        let m = env.path("marker");
        commit(&row, "work.txt", "the piece's work\n");
        let line = format!("once (1m): echo once >> {}", m.display());
        put(&store, vec![done(1, 1, None, &[&line])]);
        // `piece done` has run it at this commit already.
        let sha = git(&row, &["rev-parse", "HEAD"]).unwrap().trim().to_string();
        let cmd = check::step_of(&line, Of::Task).unwrap().command;
        let g = greens::Greens::open(&store, "p", &repo);
        g.record_by(&sha, crate::affected::key(&row, &sha, &cmd).as_deref(), &cmd, greens::Env::Warm, 1, "p-001", false);

        let out = close_with(&store, "p-001", 1, &nobody);
        assert_eq!(out, Closed::Clean { merged: vec![], ran: 0, reused: 1, removed: vec![], kept: vec![] });
        assert_eq!(git(&row, &["rev-parse", "HEAD"]).unwrap().trim(), sha, "something was merged");
        assert!(marker_lines(&m).is_empty(), "the check ran again");
    }

    #[test]
    fn a_tree_with_an_untracked_file_is_kept_and_named() {
        let (_env, store, repo, row) = scratch("litter");
        let a = piece_tree(&repo, 1, "a.txt", "from a\n");
        let b = piece_tree(&repo, 2, "b.txt", "from b\n");
        std::fs::write(repo.join(WORKTREES).join(&b).join("notes.md"), "mine\n").unwrap();
        put(&store, vec![done(1, 1, Some(&a), &[]), done(2, 1, Some(&b), &[])]);

        let out = close_with(&store, "p-001", 1, &nobody);
        let Closed::Clean { merged, removed, kept, .. } = out else { panic!("{out:?}") };
        assert_eq!(merged, vec![1, 2]);
        assert_eq!(removed.len(), 1);
        assert_eq!(kept.len(), 1, "{kept:?}");
        assert!(kept[0].0.ends_with("p-001-2") && kept[0].1.contains("notes.md"), "{kept:?}");
        assert!(repo.join(WORKTREES).join(&b).join("notes.md").exists());
        assert!(!repo.join(WORKTREES).join(&a).exists());
        assert!(row.join("b.txt").exists());
    }

    /// wsp-532: `piece done` closes the batch while the piece's seat is still in its
    /// tree, so the close keeps the tree; once the seat ends the sweep takes it, by
    /// the same rule, and a tree with residue is still kept.
    #[test]
    fn a_tree_kept_for_a_standing_seat_goes_once_the_seat_ends() {
        let (_env, store, repo, row) = scratch("seat");
        let a = piece_tree(&repo, 1, "a.txt", "from a\n");
        let b = piece_tree(&repo, 2, "b.txt", "from b\n");
        std::fs::write(repo.join(WORKTREES).join(&b).join("notes.md"), "mine\n").unwrap();
        put(&store, vec![done(1, 1, Some(&a), &[]), done(2, 1, Some(&b), &[])]);

        let seated = |dir: &Path| dir.display().to_string().contains("p-001-").then(|| "cpd-4 is standing in it".to_string());
        let out = close_with(&store, "p-001", 1, &seated);
        let Closed::Clean { removed, kept, .. } = out else { panic!("{out:?}") };
        assert!(removed.is_empty() && kept.len() == 2, "{removed:?} {kept:?}");

        // The run writes the batch closed; the seat is still there, so nothing goes.
        let mut t = store.task("p-001").unwrap();
        let mut rec = pieces::record(&t);
        rec.closed.push(1);
        pieces::write(&mut t, &rec);
        store.save_task(&t).unwrap();
        let held = crate::leg_tree::sweep(&store, &seated);
        assert!(held.iter().all(|t| matches!(t, Tree::Kept { .. })), "a tree went with its seat standing");
        assert!(repo.join(WORKTREES).join(&a).exists());

        // The tick ends the seat: the clean tree and its branch go, the one with residue is kept and named.
        let out = crate::leg_tree::sweep(&store, &nobody);
        assert_eq!(out.len(), 2, "{} trees", out.len());
        assert!(out.iter().any(|t| matches!(t, Tree::Removed { path, .. } if path.ends_with("p-001-1"))), "p-001-1 did not go");
        assert!(out.iter().any(|t| matches!(t, Tree::Kept { path, why } if path.ends_with("p-001-2") && why.contains("notes.md"))), "p-001-2 was not kept and named");
        assert!(!repo.join(WORKTREES).join(&a).exists());
        assert!(git(&repo, &["rev-parse", "--verify", "--quiet", &format!("refs/heads/{a}")]).is_none(), "the branch stayed");
        assert!(repo.join(WORKTREES).join(&b).join("notes.md").exists());
        assert!(row.join("a.txt").exists() && row.join("b.txt").exists());
    }

    #[test]
    fn a_batch_with_a_piece_still_to_end_does_not_close_and_somebody_in_the_tree_holds_it() {
        let (_env, store, repo, _row) = scratch("held");
        let a = piece_tree(&repo, 1, "a.txt", "from a\n");
        let mut running = done(2, 1, None, &[]);
        running.state = State::Running;
        put(&store, vec![done(1, 1, Some(&a), &[]), running]);
        assert!(matches!(close_with(&store, "p-001", 1, &nobody), Closed::Open(_)));

        put(&store, vec![done(1, 1, Some(&a), &[]), done(2, 1, Some(&piece_tree(&repo, 2, "b.txt", "b\n")), &[])]);
        let busy = |_: &Path| Some("p-001 is standing in it".to_string());
        let out = close_with(&store, "p-001", 1, &busy);
        assert!(matches!(&out, Closed::Held(why) if why.contains("standing")), "{out:?}");
        assert!(repo.join(WORKTREES).join(&a).exists(), "a held close took a tree");
    }
}
