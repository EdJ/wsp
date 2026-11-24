//! A by-merge row lands when its commits are on the remote's trunk (`wsp-350`,
//! EdJ/wsp#6). Nobody runs a verb for a reviewer's merge, so the daemon's pass
//! fetches the remote's trunk and reads the row's ready commit against it, and
//! writes the landing `wsp land` would have written.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::sync::Mutex;

use wsp_base::calls::Command;

use crate::cmd_checkout::{candidates, trunk, trunk_branch};
use crate::model::{project_of, Task, Worklist};
use crate::repair::LANDED;
use crate::store::Store;
use crate::util;
use crate::worklist::Position;

/// The note on a landing line naming the branch tip it covers: a squash or
/// rebase merge leaves that tip off the trunk, and [`covers`] reads it.
const MERGED: &str = "· merged";

/// How often a repository's remote is fetched.
const FETCH_EVERY: i64 = 60;

static FETCHED: Mutex<Option<HashMap<(PathBuf, String), i64>>> = Mutex::new(None);

/// Whether a landing on the row already covers the branch tip `tip`.
pub(crate) fn covers(t: &Task, tip: &str) -> bool {
    let note = format!("{MERGED} {}", tip.trim());
    t.section("Log").is_some_and(|log| log.lines().any(|l| l.contains(LANDED) && l.contains(&note)))
}

/// Lands every settled by-merge member of the current group whose work the
/// remote's trunk now holds.
pub(crate) fn land_merged(store: &Store, w: &Worklist, at: usize, pos: &Position) {
    for s in pos.members.iter().filter(|s| s.settlement.settled()) {
        if !crate::lands::by_merge(store, &s.id) {
            continue;
        }
        let Some(t) = store.find_task(&s.id) else { continue };
        let Some(ready) = crate::repair::ready(&t) else { continue };
        land_one(store, w, at, &t, &ready);
    }
}

/// What the remote's trunk holds of a row.
struct Merged {
    /// The row's own commit there.
    sha: String,
    /// What was there before it.
    base: String,
    /// The row's tip that it covers.
    tip: String,
}

fn land_one(store: &Store, w: &Worklist, at: usize, t: &Task, ready: &str) {
    let id = t.id.clone();
    let Some(repo) = candidates(store, Path::new("/"), &id)
        .into_iter()
        .find(|r| run(r, &["cat-file", "-e", &format!("{ready}^{{commit}}")]).is_some())
    else {
        return;
    };
    let Some(trunk_dir) = trunk(&repo) else { return };
    let Some(on) = trunk_branch(&trunk_dir) else { return };
    let remote = project_of(t)
        .and_then(|p| store.project(&p))
        .map_or_else(|| crate::model::DEFAULT_REMOTE.to_string(), |p| p.remote().to_string());
    let upstream = format!("refs/remotes/{remote}/{on}");
    fetch(&trunk_dir, &remote, &on);
    let Some(m) = merged(&trunk_dir, &upstream, &on, &id, ready) else { return };

    let line = format!("{LANDED} {} on {on}", m.sha);
    let wrote = store.locked(|| {
        let Some(mut t) = store.find_task(&id) else { return false };
        if crate::repair::ready(&t).as_deref() != Some(ready) {
            return false;
        }
        t.log(&format!("{line} · since {} {MERGED} {}", m.base, m.tip));
        t.touch();
        store.save_task(&t).is_ok()
    });
    if !wrote {
        return;
    }
    store.git_commit(&format!("wsp: recorded a landing of {id}"));

    // Only the clean fast-forward: a trunk with commits or edits of its own is
    // left as it is, and said once, with the landing.
    let behind = match crate::cmd_verify::git_ok(&trunk_dir, &["merge", "--ff-only", "--quiet", &upstream]) {
        Ok(()) => String::new(),
        Err(e) => format!(
            "; {} was not fast-forwarded to {remote}/{on}: {}",
            util::contract(&trunk_dir),
            e.lines().next().unwrap_or_default()
        ),
    };
    let said = format!("{id} merged on {remote}/{on} at {}: wsp recorded the landing{behind}", m.sha);
    crate::repair::stamp(store, &format!("{} group {at}: {said}", w.id));
    crate::cycle::tell(store, w, &format!("{said}. The {} run moves on.", w.id));
    crate::cycle::poke_task(store, &id, "land");
    crate::check::after_land(store, &id);
}

/// `git fetch <remote> <branch>`, at most once a [`FETCH_EVERY`] per
/// repository. A failure is silent: the tracking ref stays as it was.
fn fetch(repo: &Path, remote: &str, branch: &str) {
    let now = util::epoch_secs();
    {
        let mut held = FETCHED.lock().unwrap_or_else(|e| e.into_inner());
        let held = held.get_or_insert_with(HashMap::new);
        let key = (repo.to_path_buf(), remote.to_string());
        if held.get(&key).is_some_and(|last| now - last < FETCH_EVERY) {
            return;
        }
        held.insert(key, now);
    }
    // No prompt: the daemon has nobody to answer one.
    let _ = Command::new("git")
        .arg("-C")
        .arg(repo)
        .args(["fetch", "--quiet", remote, &format!("+refs/heads/{branch}:refs/remotes/{remote}/{branch}")])
        .env("GIT_TERMINAL_PROMPT", "0")
        .env_remove("GIT_INDEX_FILE")
        .stdin(Stdio::null())
        .output();
}

fn run(repo: &Path, args: &[&str]) -> Option<String> {
    crate::cmd_verify::git(repo, args).map(|s| s.trim().to_string())
}

fn lines(repo: &Path, args: &[&str]) -> Vec<String> {
    run(repo, args).map(|s| s.lines().map(str::to_string).collect()).unwrap_or_default()
}

fn is_ancestor(repo: &Path, a: &str, b: &str) -> bool {
    run(repo, &["merge-base", "--is-ancestor", a, b]).is_some()
}

fn merged(repo: &Path, upstream: &str, on: &str, id: &str, ready: &str) -> Option<Merged> {
    let ready = run(repo, &["rev-parse", "--verify", "--quiet", &format!("{ready}^{{commit}}")])?;
    let first_parents = lines(repo, &["rev-list", "--first-parent", upstream]);
    if is_ancestor(repo, &ready, upstream) {
        return Some(contained(repo, upstream, on, &first_parents, ready));
    }
    // The branch moved past the ready line and was merged. Only a commit off
    // the first-parent line: a branch reset to the trunk holds no work.
    if let Some(head) = run(repo, &["rev-parse", "--verify", "--quiet", &format!("refs/heads/{id}")]) {
        if is_ancestor(repo, &head, upstream) && !first_parents.contains(&head) {
            return Some(contained(repo, upstream, on, &first_parents, head));
        }
    }
    let base = run(repo, &["merge-base", upstream, &ready])?;
    // Rebased or one-commit squash: every commit of the row has its patch there.
    let cherry = lines(repo, &["cherry", upstream, &ready]);
    let each = !cherry.is_empty() && cherry.iter().all(|l| l.starts_with('-'));
    let theirs = patch_ids(repo, &["log", "-p", "--no-merges", &format!("{base}..{upstream}")]);
    let sha = if each {
        let last = patch_ids(repo, &["log", "-p", "-1", &ready]).pop()?.0;
        let sha = theirs.iter().find(|(p, _)| *p == last)?.1.clone();
        let n = cherry.len();
        return Some(Merged { base: run(repo, &["rev-parse", &format!("{sha}~{n}")])?, sha, tip: ready });
    } else {
        // Several commits squashed into one: the row's whole diff is one patch.
        let whole = patch_ids(repo, &["diff", &base, &ready]).pop()?.0;
        theirs.iter().find(|(p, _)| *p == whole)?.1.clone()
    };
    Some(Merged { base: run(repo, &["rev-parse", &format!("{sha}^")])?, sha, tip: ready })
}

/// `tip` is on the remote's trunk: by which commit, and over what.
fn contained(repo: &Path, upstream: &str, on: &str, first_parents: &[String], tip: String) -> Merged {
    if !first_parents.contains(&tip) {
        // The oldest first-parent commit holding it is the merge that brought it.
        let since = lines(repo, &["rev-list", "--first-parent", "--reverse", &format!("{tip}..{upstream}")]);
        if let Some(sha) = since.into_iter().find(|c| is_ancestor(repo, &tip, c)) {
            let base = run(repo, &["rev-parse", &format!("{sha}^")]).unwrap_or_else(|| sha.clone());
            return Merged { sha, base, tip };
        }
    }
    // A fast-forward: the tip itself, since where it left the local trunk.
    let base = run(repo, &["merge-base", on, &tip])
        .filter(|b| *b != tip)
        .or_else(|| run(repo, &["rev-parse", &format!("{tip}~1")]))
        .unwrap_or_else(|| tip.clone());
    Merged { sha: tip.clone(), base, tip }
}

/// `(patch id, commit)` for each patch `git <args>` prints.
fn patch_ids(repo: &Path, args: &[&str]) -> Vec<(String, String)> {
    let patch = || -> Option<String> {
        let diff = crate::cmd_verify::git(repo, args)?;
        let mut child = Command::new("git")
            .arg("-C")
            .arg(repo)
            .args(["patch-id", "--stable"])
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()
            .ok()?;
        let mut stdin = child.stdin.take()?;
        let writer = std::thread::spawn(move || std::io::Write::write_all(&mut stdin, diff.as_bytes()));
        let out = child.wait_with_output().ok()?;
        writer.join().ok()?.ok()?;
        Some(String::from_utf8_lossy(&out.stdout).into_owned())
    };
    patch()
        .unwrap_or_default()
        .lines()
        .filter_map(|l| l.split_once(' ').map(|(p, c)| (p.to_string(), c.to_string())))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{Group, Project, Status, WorklistStatus};
    use crate::worklist::{self, Reading};
    use wsp_testkit::git;

    struct Fix {
        _env: util::Isolated,
        store: Store,
        repo: PathBuf,
        /// A second clone of the origin, where the reviewer merges.
        reviewer: PathBuf,
        ready: String,
        list: Worklist,
    }

    fn commit(dir: &Path, file: &str) -> String {
        std::fs::write(dir.join(file), file).unwrap();
        git(dir, &["add", file]);
        git(dir, &["commit", "--quiet", "-m", file]);
        git(dir, &["rev-parse", "HEAD"]).trim().to_string()
    }

    /// A verified-ready `m-1` of `work` commits, its branch pushed as `pushed`.
    fn fix(tag: &str, by_merge: bool, work: &[&str], pushed: &str) -> Fix {
        let (env, store) = crate::testkit::isolated_store(&format!("by-merge-{tag}"));
        let (origin, repo, reviewer) = (env.path("origin.git"), env.path("repo"), env.path("reviewer"));
        std::fs::create_dir_all(&origin).unwrap();
        git(&origin, &["init", "--quiet", "--bare", "-b", "master"]);
        crate::testkit::repo(&repo);
        commit(&repo, "first");
        git(&repo, &["remote", "add", "origin", &origin.display().to_string()]);
        git(&repo, &["push", "--quiet", "origin", "master"]);
        git(&repo, &["checkout", "--quiet", "-b", "m-1"]);
        let mut ready = String::new();
        for f in work {
            ready = commit(&repo, f);
        }
        git(&repo, &["push", "--quiet", "origin", &format!("m-1:{pushed}")]);
        git(&repo, &["checkout", "--quiet", "master"]);
        crate::testkit::repo(&reviewer);
        git(&reviewer, &["remote", "add", "origin", &origin.display().to_string()]);
        git(&reviewer, &["fetch", "--quiet", "origin"]);
        git(&reviewer, &["reset", "--quiet", "--hard", "origin/master"]);

        let mut p = Project::new("p");
        p.roots = vec![repo.display().to_string()];
        if by_merge {
            p.lands = crate::lands::BY_MERGE.into();
        }
        store.save_project(&p).unwrap();
        let mut t = Task::new("m-1", "m-1");
        t.project = Some("p".into());
        t.set_status(Status::Review);
        t.log(&format!("{} {ready} on m-1", crate::repair::READY));
        store.save_task(&t).unwrap();
        let mut list = Worklist::new("run", "run");
        list.set_status(WorklistStatus::Running);
        list.set_groups(&[Group { members: vec!["m-1".into()], agent: "claude".into(), ..Group::default() }]);
        store.save_worklist(&list).unwrap();
        Fix { _env: env, store, repo, reviewer, ready, list }
    }

    impl Fix {
        fn pass(&self) {
            let pos = worklist::position(&self.store, &self.list, Reading::Landed);
            land_merged(&self.store, &self.list, 1, &pos);
        }

        fn log(&self) -> String {
            self.store.find_task("m-1").unwrap().section("Log").unwrap_or_default()
        }

        fn merge(&self, from: &str, squash: bool) -> String {
            git(&self.reviewer, &["fetch", "--quiet", "origin"]);
            commit(&self.reviewer, "someone-else");
            let from = format!("origin/{from}");
            if squash {
                git(&self.reviewer, &["merge", "--quiet", "--squash", &from]);
                git(&self.reviewer, &["commit", "--quiet", "-m", "squashed PR"]);
            } else {
                git(&self.reviewer, &["merge", "--quiet", "--no-ff", "-m", "merge PR", &from]);
            }
            let sha = git(&self.reviewer, &["rev-parse", "HEAD"]).trim().to_string();
            commit(&self.reviewer, "later");
            git(&self.reviewer, &["push", "--quiet", "origin", "master"]);
            sha
        }

        fn landed_as(&self, sha: &str) {
            let log = self.log();
            assert!(log.contains(&format!("{LANDED} {sha} on master · since ")), "{log}");
            let t = self.store.find_task("m-1").unwrap();
            assert_eq!(crate::repair::ready(&t), None);
            let pos = worklist::position(&self.store, &self.list, Reading::Landed);
            assert!(pos.members[0].finished(), "the run moves on: {pos:?}");
            let head = git(&self.repo, &["rev-parse", "master"]);
            assert_eq!(head, git(&self.reviewer, &["rev-parse", "master"]), "the local trunk follows");
        }
    }

    #[test]
    fn a_branch_merged_on_origin_lands_with_the_merge_commit() {
        let f = fix("merge", true, &["work"], "m-1");
        let sha = f.merge("m-1", false);
        f.pass();
        f.landed_as(&sha);
        let after = f.log();
        f.pass();
        assert_eq!(f.log(), after, "once");
    }

    #[test]
    fn a_pr_from_another_branch_squashed_on_origin_lands() {
        let f = fix("squash", true, &["work"], "pr-other");
        let sha = f.merge("pr-other", true);
        f.pass();
        f.landed_as(&sha);
    }

    #[test]
    fn several_commits_squashed_into_one_land() {
        let f = fix("squash-two", true, &["one", "two"], "pr-other");
        let sha = f.merge("pr-other", true);
        f.pass();
        f.landed_as(&sha);
    }

    #[test]
    fn an_unmerged_row_is_not_landed_and_nothing_is_logged() {
        let f = fix("unmerged", true, &["work"], "m-1");
        let before = f.log();
        crate::cycle::tests::SAID.with(|s| s.borrow_mut().clear());
        f.pass();
        f.pass();
        assert_eq!(f.log(), before);
        assert!(crate::cycle::tests::SAID.with(|s| s.borrow().is_empty()));
        assert!(before.contains(&f.ready) && !before.contains(LANDED), "{before}");
    }

    #[test]
    fn without_the_field_nothing_is_fetched_or_landed() {
        let f = fix("off", false, &["work"], "m-1");
        f.merge("m-1", false);
        let tracked = git(&f.repo, &["rev-parse", "origin/master"]);
        let before = f.log();
        f.pass();
        assert_eq!(git(&f.repo, &["rev-parse", "origin/master"]), tracked, "no fetch");
        assert_eq!(f.log(), before);
    }
}
