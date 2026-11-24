//! A by-merge row lands when its commits are on the remote's trunk (`wsp-350`,
//! EdJ/wsp#6). Nobody runs a verb for a reviewer's merge, so the daemon's pass
//! fetches the remote's trunk and reads the row's ready commit against it, and
//! writes the landing `wsp land` would have written.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::sync::Mutex;
use std::time::{Duration, Instant};

use wsp_base::calls::Command;

use crate::cmd_checkout::{candidates, member_tree, open_pr_json, trunk, trunk_branch};
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

/// How long one fetch may take before it is killed: the daemon's pass over
/// every project waits for it.
const FETCH_LIMIT: Duration = Duration::from_secs(30);

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

/// The repository holding a row's ready commit, and what its PR merges into.
struct Base {
    repo: PathBuf,
    trunk_dir: PathBuf,
    on: String,
    remote: String,
}

fn base(store: &Store, t: &Task, ready: &str) -> Option<Base> {
    let repo = candidates(store, Path::new("/"), &t.id)
        .into_iter()
        .find(|r| run(r, &["cat-file", "-e", &format!("{ready}^{{commit}}")]).is_some())?;
    let trunk_dir = trunk(&repo)?;
    let on = trunk_branch(&trunk_dir)?;
    let remote = project_of(t)
        .and_then(|p| store.project(&p))
        .map_or_else(|| crate::model::DEFAULT_REMOTE.to_string(), |p| p.remote().to_string());
    Some(Base { repo, trunk_dir, on, remote })
}

/// `<remote>/<trunk>`: what a by-merge row's change is compared against.
pub(crate) fn base_ref(store: &Store, t: &Task) -> Option<String> {
    let b = base(store, t, &crate::repair::ready(t)?)?;
    Some(format!("{}/{}", b.remote, b.on))
}

/// The commit a by-merge row's verifier reads.
pub(crate) struct Head {
    pub sha: String,
    /// The PR's url, or the remote branch; `None` for the ready commit.
    pub from: Option<String>,
}

/// The head of the row's PR: by `gh` on a GitHub remote, else the remote's
/// branch of the same name after a fetch, else the ready commit. Fetches the
/// remote's trunk too, so the comparison base is current.
pub(crate) fn pr_head(store: &Store, t: &Task, ready: &str) -> Option<Head> {
    let b = base(store, t, ready)?;
    let dir = member_tree(store, &t.id).unwrap_or_else(|| b.repo.clone());
    let branch = run(&dir, &["symbolic-ref", "--quiet", "--short", "HEAD"]).filter(|x| *x != b.on).unwrap_or_else(|| t.id.clone());
    fetch_now(&b.trunk_dir, &b.remote, &b.on);
    let has = |sha: &str| run(&b.repo, &["cat-file", "-e", &format!("{sha}^{{commit}}")]).is_some();
    if let Some(pr) = open_pr_json(store, &t.id, &dir, &branch, "headRefOid,headRefName,url") {
        let said = |k: &str| pr.get(k).and_then(|v| v.as_str()).map(str::to_string);
        if let (Some(sha), Some(name), Some(url)) = (said("headRefOid"), said("headRefName"), said("url")) {
            fetch_now(&b.trunk_dir, &b.remote, &name);
            if has(&sha) {
                return Some(Head { sha, from: Some(url) });
            }
        }
    }
    // Only a branch the fetch just reached: a tracking ref left from before
    // may be older than the work.
    if fetch_now(&b.trunk_dir, &b.remote, &branch) {
        let tracked = format!("refs/remotes/{}/{branch}", b.remote);
        if let Some(sha) = run(&b.repo, &["rev-parse", "--verify", "--quiet", &tracked]) {
            return Some(Head { sha, from: Some(format!("{}/{branch}", b.remote)) });
        }
    }
    Some(Head { sha: ready.to_string(), from: None })
}

fn land_one(store: &Store, w: &Worklist, at: usize, t: &Task, ready: &str) {
    let id = t.id.clone();
    let Some(Base { trunk_dir, on, remote, .. }) = base(store, t, ready) else { return };
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
    fetch_now(repo, remote, branch);
}

/// [`fetch`] with no wait between two, and whether it reached the branch.
fn fetch_now(repo: &Path, remote: &str, branch: &str) -> bool {
    fetch_within(repo, remote, branch, FETCH_LIMIT)
}

/// [`fetch_now`], killed and reaped once `limit` has passed.
fn fetch_within(repo: &Path, remote: &str, branch: &str, limit: Duration) -> bool {
    // No prompt: the daemon has nobody to answer one.
    let mut git = Command::new("git");
    git.arg("-C")
        .arg(repo)
        .args(["fetch", "--quiet", remote, &format!("+refs/heads/{branch}:refs/remotes/{remote}/{branch}")])
        .env("GIT_TERMINAL_PROMPT", "0")
        .env_remove("GIT_INDEX_FILE")
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        // A group of its own, so the kill reaches git's ssh or remote helper.
        .process_group(0);
    let Ok(mut child) = git.spawn() else { return false };
    let started = Instant::now();
    loop {
        match child.try_wait() {
            Ok(Some(status)) => return status.success(),
            Ok(None) if started.elapsed() < limit => std::thread::sleep(Duration::from_millis(20)),
            _ => break,
        }
    }
    crate::host::Host::signal_group(&crate::host::LOCAL, child.id(), "KILL");
    let _ = child.kill();
    let _ = child.wait();
    false
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

    /// The reviewer's branch moved past the ready commit; then a pass opens.
    fn opened(f: &Fix) -> (String, crate::verification::Pass, String) {
        git(&f.reviewer, &["checkout", "--quiet", "-b", "m-1", "origin/m-1"]);
        let head = commit(&f.reviewer, "review-fix");
        git(&f.reviewer, &["push", "--quiet", "origin", "m-1"]);
        let on = crate::model::Policy::parse("claude").unwrap();
        let t = f.store.find_task("m-1").unwrap();
        crate::cycle::open_verifier(&f.store, &t, &on, &|_: &str| false, &|_: &str| false, &|| true).expect("a pass opens");
        let t = f.store.find_task("m-1").unwrap();
        let pass = crate::verification::passes(&t).pop().unwrap();
        (head, pass, crate::cycle::verifier_order(&f.store, &t))
    }

    #[test]
    fn a_by_merge_rows_pass_reads_the_remote_branch_head() {
        let f = fix("pass", true, &["work"], "m-1");
        let (head, pass, order) = opened(&f);
        let (read, ready) = (&head[..12], &f.ready[..12]);
        assert_eq!(pass.read.as_deref(), Some(read));
        assert_eq!(pass.ready.as_deref(), Some(ready));
        assert_eq!(pass.from.as_deref(), Some("origin/m-1"));
        assert!(order.contains(&format!("**Read commit {read}**, the head of origin/m-1")), "{order}");
        assert!(order.contains(&format!("`git diff origin/master...{read}`")), "{order}");
        assert!(order.contains(crate::lands::WAITING), "{order}");
        assert!(!order.contains("rebased on the trunk") && !order.contains("wsp land") && !order.contains("lands "), "{order}");
        // The head differing from the ready commit is not the member moving.
        let t = f.store.find_task("m-1").unwrap();
        let on = crate::model::Policy::parse("claude").unwrap();
        assert!(crate::cycle::open_verifier(&f.store, &t, &on, &|_: &str| false, &|_: &str| false, &|| true).is_none());
    }

    #[test]
    fn without_the_field_a_pass_reads_the_ready_commit_in_todays_words() {
        let f = fix("pass-off", false, &["work"], "m-1");
        let (_, pass, order) = opened(&f);
        let ready = &f.ready[..12];
        assert_eq!((pass.read.as_deref(), pass.ready, pass.from), (Some(ready), None, None));
        assert!(
            order.contains(&format!(
                "wsp started you when m-1 reached review, ready at {ready} and rebased on the trunk; it is not on the trunk yet and nobody else is going to read this work before it is.\n\n\
                 **You are read-only.** Do not edit, commit, rebase, land, or change m-1's status; never type `wsp land`. \
                 **You stand in m-1's own tree**, at the commit it is ready at, with its build output: "
            )),
            "{order}"
        );
    }

    #[test]
    fn a_fetch_from_a_remote_that_never_answers_is_given_up() {
        let f = fix("silent", true, &["work"], "m-1");
        // Connections are queued and never accepted, so git waits for a reply.
        let silent = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let url = format!("git://{}/origin.git", silent.local_addr().unwrap());
        git(&f.repo, &["remote", "set-url", "origin", &url]);
        let tracked = git(&f.repo, &["rev-parse", "origin/master"]);
        let started = Instant::now();
        assert!(!fetch_within(&f.repo, "origin", "master", Duration::from_secs(1)));
        let took = started.elapsed();
        assert!(took >= Duration::from_secs(1) && took < Duration::from_secs(10), "{took:?}");
        assert_eq!(git(&f.repo, &["rev-parse", "origin/master"]), tracked);
    }

    #[test]
    fn a_local_trunk_with_its_own_commit_is_left_and_the_row_still_lands() {
        let f = fix("diverged", true, &["work"], "m-1");
        let sha = f.merge("m-1", false);
        let local = commit(&f.repo, "only-here");
        crate::cycle::tests::SAID.with(|s| s.borrow_mut().clear());
        f.pass();
        let log = f.log();
        assert!(log.contains(&format!("{LANDED} {sha} on master · since ")), "{log}");
        assert_eq!(crate::repair::ready(&f.store.find_task("m-1").unwrap()), None);
        assert_eq!(git(&f.repo, &["rev-parse", "master"]).trim(), local);
        let said = crate::cycle::tests::SAID.with(|s| s.borrow().join("\n"));
        assert!(said.contains("was not fast-forwarded to origin/master"), "{said}");
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
