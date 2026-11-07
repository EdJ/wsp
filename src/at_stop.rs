//! A tree's own teardown (`wsp-370`): the project's `at_stop:` command, run
//! once with the tree's root as its cwd, just before wsp removes the tree.
//!
//! A seat, a leg or a check can start servers and watchers of its own in its
//! tree (compounding's `spikes/web-chrome/probe/dev.sh` starts Vite and a
//! watcher and writes their pids under `.dev/`), and when the tree is taken
//! away nothing stops them. The one thing that knows how to stop them is the
//! project, so the project says: an optional `at_stop:` key in its front
//! matter, a shell command. A project without the key runs nothing, and there
//! is no default — wsp does not guess that a script it has not been told about
//! is a teardown.
//!
//! # Where it runs
//!
//! [`before`] is called from [`crate::cmd_checkout::remove`], which is the one
//! place a seat's tree and a barrier check's tree are destroyed, and from
//! [`crate::leg_tree`]'s own take (a leg's branch is judged against the row's
//! and not the trunk's, so it cannot go through `remove`). It runs while the
//! tree is still there, because the command is *in* it.
//!
//! # What it may not do
//!
//! It is bounded by [`LIMIT`], and one still running at the limit has its whole
//! process group killed. It never blocks the teardown: a failure, a timeout or
//! a command that will not start is said — to `cycle.log`, to the row's log
//! when the tree is a row's, and to stderr — and the tree goes regardless. It
//! kills nothing by pattern; ending its own processes by pid is the command's
//! business (`dev.sh stop` does).

use std::path::Path;
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

use crate::store::Store;
use crate::util;

/// How long the command has before its process group is killed.
pub(crate) const LIMIT: Duration = Duration::from_secs(30);

/// How much of the end of the command's output is kept in what is said.
const TAIL_LINES: usize = 5;
const TAIL_CHARS: usize = 400;

/// How a teardown command ended.
#[derive(Debug, PartialEq)]
pub(crate) enum Ended {
    Exited(i32),
    /// Killed by a signal that was not ours.
    Signalled,
    /// Still running at the limit, and killed.
    TimedOut,
    /// `sh` could not be started.
    NotStarted(String),
}

/// What running the command came to: how it ended and the tail of its output.
#[derive(Debug)]
pub(crate) struct Ran {
    pub ended: Ended,
    pub tail: String,
}

/// Run `command` under `sh -c` in `dir`, its output to a file read back for its
/// tail. A file and not a pipe: a command that leaves a daemon running holds a
/// pipe's write end open, and reading to the end would wait on the daemon.
pub(crate) fn run_command(dir: &Path, command: &str, limit: Duration) -> Ran {
    use std::os::unix::process::CommandExt;
    static N: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
    let log = std::env::temp_dir().join(format!(
        "wsp-at-stop-{}-{}.log",
        std::process::id(),
        N.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
    ));
    let not_started = |why: String| Ran { ended: Ended::NotStarted(why), tail: String::new() };
    let Ok(out) = std::fs::File::create(&log) else { return not_started(format!("could not write {}", log.display())) };
    let Ok(err) = out.try_clone() else { return not_started("could not share the log".into()) };
    let mut cmd = Command::new("sh");
    cmd.arg("-c").arg(command).current_dir(dir).stdin(Stdio::null()).stdout(out).stderr(err).process_group(0);
    crate::check::clean_env(&mut cmd);
    let mut child = match cmd.spawn() {
        Ok(c) => c,
        Err(e) => {
            let _ = std::fs::remove_file(&log);
            return not_started(format!("sh: {e}"));
        }
    };
    let start = Instant::now();
    let ended = loop {
        match child.try_wait() {
            Ok(Some(s)) => break s.code().map(Ended::Exited).unwrap_or(Ended::Signalled),
            Ok(None) if start.elapsed() >= limit => {
                // The group, because `sh -c` runs the command as a child of
                // its own and killing only `sh` would leave that running.
                let _ = Command::new("kill").args(["-s", "KILL", "--", &format!("-{}", child.id())]).status();
                let _ = child.kill();
                let _ = child.wait();
                break Ended::TimedOut;
            }
            Ok(None) => std::thread::sleep(Duration::from_millis(20)),
            Err(e) => break Ended::NotStarted(format!("wait: {e}")),
        }
    };
    let text = std::fs::read_to_string(&log).unwrap_or_default();
    let _ = std::fs::remove_file(&log);
    Ran { ended, tail: tail_of(&text) }
}

/// The last few lines of `text`, and no more than a few hundred characters.
fn tail_of(text: &str) -> String {
    let lines: Vec<&str> = text.lines().map(str::trim_end).filter(|l| !l.trim().is_empty()).collect();
    let from = lines.len().saturating_sub(TAIL_LINES);
    let joined = lines[from..].join(" | ");
    let skip = joined.chars().count().saturating_sub(TAIL_CHARS);
    joined.chars().skip(skip).collect()
}

/// The line that says what happened, for the logs. `None` for a clean exit
/// with nothing to say, which is what a teardown that worked looks like.
fn said(path: &str, ran: &Ran) -> String {
    let how = match &ran.ended {
        Ended::Exited(0) => "exit 0".to_string(),
        Ended::Exited(n) => format!("FAILED, exit {n}"),
        Ended::Signalled => "FAILED, killed by a signal".to_string(),
        Ended::TimedOut => format!("FAILED, cut off after {}s", LIMIT.as_secs()),
        Ended::NotStarted(why) => format!("FAILED, did not start: {why}"),
    };
    match ran.tail.is_empty() {
        true => format!("at_stop in {path}: {how}"),
        false => format!("at_stop in {path}: {how}; output ends: {}", ran.tail),
    }
}

/// The `at_stop:` of the project whose repository `repo` is (or is a tree of).
fn command_for(store: &Store, repo: &Path) -> Option<String> {
    let trunk = crate::cmd_checkout::trunk(repo).unwrap_or_else(|| repo.to_path_buf());
    let trunk = util::real(&trunk.display().to_string());
    store
        .projects()
        .into_iter()
        .filter(|p| !p.at_stop.trim().is_empty())
        .find(|p| {
            p.roots
                .iter()
                .filter_map(|r| crate::check_tree::repo_of(r))
                .any(|r| util::real(&r.display().to_string()) == trunk)
        })
        .map(|p| p.at_stop)
}

/// Whether the project of `t` declares an `at_stop:`, and so its seat may have
/// servers of its own that only the teardown stops (`wsp-430`).
pub(crate) fn declared(store: &Store, t: &crate::model::Task) -> bool {
    let mut at = crate::check::project_of(t);
    while let Some(id) = at.take() {
        let Some(p) = store.project(&id) else { return false };
        if !p.at_stop.trim().is_empty() {
            return true;
        }
        at = p.parent.clone();
    }
    false
}

/// Run the project's teardown for the tree at `dir`, of the repository `repo`,
/// then say what it came to. Does nothing when the tree is not there, when
/// nothing in the store says `repo` is a project's, or when that project has no
/// `at_stop:`.
pub(crate) fn run_for(store: &Store, repo: &Path, dir: &Path, limit: Duration) -> Option<Ran> {
    if !dir.join(".git").exists() {
        return None;
    }
    let command = command_for(store, repo)?;
    let ran = run_command(dir, &command, limit);
    let path = util::contract(dir);
    let line = said(&path, &ran);
    crate::store::log_line(store, &line);
    // A seat's tree is named for its row; a check's or a leg's is not, and has
    // no row of its own to write to.
    let name = dir.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
    if let Some(mut task) = store.task(&name) {
        let _ = crate::log::note(store, &mut task, &line);
    }
    if ran.ended != Ended::Exited(0) {
        eprintln!("wsp: {line}");
    }
    Some(ran)
}

/// The call every tree's removal makes first. Opens the store itself, because
/// the removal does not carry one.
pub(crate) fn before(repo: &Path, dir: &Path) {
    // A test that is not about this has no store of its own to open, and
    // opening the live one is refused (see [`Store::open`]).
    if cfg!(test) && (std::env::var_os("WSP_HOME").is_none() || std::env::var_os("WSP_STATE").is_none()) {
        return;
    }
    let store = Store::open();
    run_for(&store, repo, dir, limit());
}

#[cfg(test)]
thread_local! {
    pub(crate) static TEST_LIMIT: std::cell::Cell<Option<Duration>> = const { std::cell::Cell::new(None) };
}

fn limit() -> Duration {
    #[cfg(test)]
    if let Some(l) = TEST_LIMIT.with(|l| l.get()) {
        return l;
    }
    LIMIT
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::Project;
    use std::path::PathBuf;

    fn git(dir: &Path, args: &[&str]) {
        let out = Command::new("git")
            .arg("-C")
            .arg(dir)
            .args(args)
            .env("GIT_AUTHOR_NAME", "t")
            .env("GIT_AUTHOR_EMAIL", "t@t")
            .env("GIT_COMMITTER_NAME", "t")
            .env("GIT_COMMITTER_EMAIL", "t@t")
            .output()
            .unwrap();
        assert!(out.status.success(), "git {args:?}: {}", String::from_utf8_lossy(&out.stderr));
    }

    /// A scratch repository with a project rooted at it, carrying `at_stop`,
    /// and a linked tree of it called `name`.
    fn scratch(name: &str, at_stop: &str) -> (util::Isolated, Store, PathBuf, PathBuf) {
        let env = util::isolated(&format!("at-stop-{name}"));
        let repo = env.path("repo");
        std::fs::create_dir_all(&repo).unwrap();
        crate::testkit::repo(&repo);
        std::fs::write(repo.join(".gitignore"), "target/\n.worktrees/\n").unwrap();
        git(&repo, &["add", ".gitignore"]);
        git(&repo, &["commit", "--quiet", "-m", "first"]);
        let store = Store::open();
        let mut p = Project::new("p");
        p.roots = vec![repo.display().to_string()];
        p.at_stop = at_stop.to_string();
        store.save_project(&p).unwrap();
        let dir = repo.join(".worktrees").join("p-001");
        git(&repo, &["worktree", "add", "--quiet", "-b", "p-001", &dir.display().to_string(), "master"]);
        (env, store, repo, dir)
    }

    #[test]
    fn the_key_survives_a_round_trip_and_is_absent_when_unset() {
        let mut p = Project::new("x");
        assert!(!p.render().contains("at_stop"), "an empty key was written");
        p.at_stop = "[ -x a ] && a stop".into();
        let back = Project::from_doc(&crate::fm::parse(&p.render()), "x");
        assert_eq!(back.at_stop, "[ -x a ] && a stop");
    }

    /// The seat's end: removing a row's tree runs the project's command in it,
    /// with the tree's root as cwd, while the tree is still there.
    #[test]
    fn a_seats_tree_gets_its_marker_before_it_goes() {
        let (env, store, repo, dir) = scratch("seat", "pwd > \"$MARKER_DIR/marker\"; ls .git > /dev/null && echo here >> \"$MARKER_DIR/marker\"");
        std::env::set_var("MARKER_DIR", env.path(""));
        let mut t = crate::model::Task::new("a row", "p-001");
        t.project = Some("p".into());
        store.save_task(&t).unwrap();

        crate::cmd_checkout::remove(&repo, &dir);

        let marker = std::fs::read_to_string(env.path("marker")).expect("the command did not run");
        assert!(marker.contains("here"), "it did not run in the tree: {marker}");
        assert!(marker.lines().next().unwrap().ends_with("p-001"), "{marker}");
        assert!(!dir.exists(), "the tree stayed");
        let row = store.task("p-001").unwrap();
        assert!(row.body.contains("at_stop in") && row.body.contains("exit 0"), "the row's log: {}", row.body);
    }

    /// A check tree taken down — by the tick's sweep — gets the same.
    #[test]
    fn a_check_tree_taken_down_gets_its_marker() {
        let (env, store, repo, dir) = scratch("check", "echo done > \"$MARKER_DIR/check-marker\"");
        std::env::set_var("MARKER_DIR", env.path(""));
        // `p-001` is a row here; a check tree is named for its list.
        let check = repo.join(".worktrees").join("run-check-1");
        git(&repo, &["worktree", "add", "--quiet", "-b", "run-check-1", &check.display().to_string(), "master"]);
        let mut w = crate::model::Worklist::new("run", "p");
        let mut g = crate::model::Group::default();
        g.members = vec!["p-001".into()];
        crate::barrier::push(&mut g.checks, "2026-10-17T10:00:00Z passes pane:cpd-1 ended");
        w.set_groups(&[g]);
        store.save_worklist(&w).unwrap();
        let _ = dir;

        let out = crate::check_tree::sweep(&store, &|_: &Path| None);
        assert!(out.iter().any(|t| matches!(t, crate::cmd_checkout::Tree::Removed { .. })), "no tree was taken");
        assert!(!check.exists());
        assert!(env.path("check-marker").exists(), "the check tree's teardown did not run");
    }

    #[test]
    fn a_projects_without_the_key_runs_nothing() {
        let (env, store, repo, dir) = scratch("none", "");
        std::env::set_var("MARKER_DIR", env.path(""));
        assert!(run_for(&store, &repo, &dir, LIMIT).is_none(), "a project without the key ran something");
        crate::cmd_checkout::remove(&repo, &dir);
        assert!(!dir.exists());
        assert!(!store.task("p-001").is_some_and(|t| t.body.contains("at_stop")));
    }

    #[test]
    fn a_repository_no_project_owns_runs_nothing() {
        let (_env, store, _repo, _dir) = scratch("stranger", "touch ran");
        let other = _env.path("other");
        std::fs::create_dir_all(&other).unwrap();
        crate::testkit::repo(&other);
        git(&other, &["commit", "--quiet", "--allow-empty", "-m", "first"]);
        let tree = other.join("t");
        git(&other, &["worktree", "add", "--quiet", "-b", "t", &tree.display().to_string(), "master"]);
        assert!(run_for(&store, &other, &tree, LIMIT).is_none());
        assert!(!tree.join("ran").exists());
    }

    /// A command that hangs is cut off at the limit — and the teardown still
    /// completes, the tree gone and the cut-off said.
    #[test]
    fn a_hanging_command_is_cut_off_and_the_teardown_completes() {
        let (_env, store, repo, dir) = scratch("hang", "sleep 60 & sleep 60");
        TEST_LIMIT.with(|l| l.set(Some(Duration::from_millis(300))));
        let start = Instant::now();
        crate::cmd_checkout::remove(&repo, &dir);
        TEST_LIMIT.with(|l| l.set(None));
        assert!(start.elapsed() < Duration::from_secs(20), "it waited out the hang: {:?}", start.elapsed());
        assert!(!dir.exists(), "a hung command held the tree");
        let _ = store;
    }

    #[test]
    fn a_failure_is_said_with_its_exit_and_the_end_of_its_output_and_blocks_nothing() {
        let (_env, store, repo, dir) = scratch("fail", "echo one; echo two >&2; exit 3");
        let ran = run_for(&store, &repo, &dir, LIMIT).unwrap();
        assert_eq!(ran.ended, Ended::Exited(3));
        assert!(ran.tail.contains("one") && ran.tail.contains("two"), "{}", ran.tail);
        let line = said("t", &ran);
        assert!(line.contains("FAILED, exit 3") && line.contains("two"), "{line}");
        crate::cmd_checkout::remove(&repo, &dir);
        assert!(!dir.exists());
    }

    #[test]
    fn a_hung_command_reports_a_timeout_and_its_group_is_killed() {
        let env = util::isolated("at-stop-timeout");
        let marker = env.path("late");
        let ran = run_command(&env.path(""), &format!("(sleep 2; touch {}) & sleep 60", marker.display()), Duration::from_millis(300));
        assert_eq!(ran.ended, Ended::TimedOut);
        std::thread::sleep(Duration::from_millis(2500));
        assert!(!marker.exists(), "the command's child outlived the cut-off");
    }

    #[test]
    fn the_tail_is_the_last_lines_and_bounded() {
        assert_eq!(tail_of("a\nb\n\nc\n"), "a | b | c");
        let many: String = (0..20).map(|n| format!("line{n}\n")).collect();
        assert_eq!(tail_of(&many), "line15 | line16 | line17 | line18 | line19");
        assert!(tail_of(&"x".repeat(5000)).chars().count() <= TAIL_CHARS);
    }
}
