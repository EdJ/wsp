//! The stash guard: a `reference-transaction` hook that refuses `git stash`.
//!
//! `refs/stash` lives in the **common** `.git`, not in a worktree. Every agent
//! in every `.worktrees/*` checkout of this repository pushes onto and pops off
//! one stack. On 2026-08-19 two agents stashed thirty-six seconds apart and
//! exchanged their entire working trees — 603 lines and an untracked file each
//! in the wrong checkout, `git stash list` empty because both had already
//! popped, and neither agent had done anything it was told not to.
//!
//! # Why a git hook and not another sentence
//!
//! `wsp commit-help` already forbade it, in the same paragraph as the recovery
//! procedure, and both agents did neither. That is not because the rule was
//! badly written. It is because **the hazard fires during investigation** —
//! hours before anything is staged, at a moment when there is no reason at all
//! to open a document about committing. A rule reaches an agent when it is read,
//! and this one is read at the wrong time by construction.
//!
//! So the question is what is in the path at the moment `git stash` is typed,
//! and the answer is: git, and nothing else. wsp is not invoked, no wsp output
//! is on screen, and no brief written hours earlier is being reread. git's
//! `reference-transaction` hook runs inside that command, sees `refs/stash` in
//! the transaction, and can refuse it. That is the whole design.
//!
//! Three properties make it the right shape rather than merely the only one:
//!
//! - **It is shared exactly the way the stash is shared.** Hooks live in the
//!   common git directory, so one file covers every worktree of the repository
//!   and every worktree made after it — including the trunk, where a person at
//!   a shell can do the same damage as an agent.
//! - **A refusal is safe.** git aborts the whole ref transaction and the working
//!   tree is untouched: the agent is exactly where it was, holding its changes,
//!   reading a sentence that names what to do instead. Verified rather than
//!   assumed, on git 2.35.1.
//! - **It is not a lock.** No shared mutable state, nothing to wait on, nothing
//!   to leave held. `worklist-011` says outright not to build one, and the
//!   reason it says so is that two agents doing ordinary things is not
//!   contention — it is two agents who needed to be told one fact.
//!
//! # What it refuses, and the one thing it must not
//!
//! It refuses the **creation** of a stash and allows the **removal** of one, and
//! that asymmetry is measured rather than tasteful. `git stash pop` applies the
//! stash to the working tree *first* and drops the ref *after*, so a refusal
//! there arrives once the damage is done — and leaves `refs/stash` present with
//! an emptied reflog, which is a state `git stash list` reports as no stashes at
//! all. A guard that creates that is worse than no guard.
//!
//! So the hook allows a transaction that sets `refs/stash` to zeroes (the last
//! entry going), or to a value already in its own reflog (a `pop` or `drop`
//! uncovering the entry beneath). Anything else is a commit that did not exist a
//! moment ago, which is a push, which is refused. `git stash apply` and `git
//! stash list` touch no ref and are never seen here at all.
//!
//! `git rebase --autostash` is untouched, which matters because `wsp land`
//! rebases: autostash keeps its commit in `.git/rebase-merge/autostash` and
//! never goes near `refs/stash`.
//!
//! # The cost, since everything in this repository pays one
//!
//! The hook runs on every ref transaction — three phases per transaction, a
//! handful of transactions per commit. It is a `/bin/sh` script whose first line
//! exits for two of the three phases, and measured on this machine it adds
//! **~13ms to a `git commit`** and nothing at all to `status`, `diff` or `log`.
//! No tokens, and nothing at session start. That is the entire price of making a
//! class of cross-agent corruption impossible rather than forbidden.
//!
//! # Getting past it
//!
//! Deliberately not named in the refusal, because an escape hatch offered at the
//! moment of refusal is an escape hatch that gets taken. `git -c
//! core.hooksPath=/dev/null stash` is it, and it is in the README where somebody
//! deciding to do this on purpose will look.

use std::path::{Path, PathBuf};

/// The line that says the file is ours, and which version of it.
///
/// Version and marker in one string so an upgrade is a content comparison and
/// not a parse: the file is either byte-for-byte what this binary would write,
/// or it is a foreign hook, or it is an older wsp guard to be replaced.
const MARK: &str = "# wsp stash guard v1";

/// The hook itself.
///
/// POSIX `sh`, no wsp in it. A hook that shelled out to `wsp` would block every
/// git operation in the repository the day `~/.local/bin/wsp` is missing or
/// half-installed, and this file exists precisely to be relied on.
fn script() -> String {
    format!(
        r#"#!/bin/sh
{MARK} — installed by wsp. Delete it and stashing works again.
# refs/stash is per-repository; every worktree shares one stack.
[ "$1" = prepared ] || exit 0
while read -r old new ref; do
	[ "$ref" = "refs/stash" ] || continue
	# All zeroes: the last entry going. Allowed — see below.
	case "$new" in *[!0]*) ;; *) continue ;; esac
	# Already in its own reflog: a pop or drop uncovering an older entry.
	# A push writes a commit that did not exist a moment ago, so it is not.
	log="$(git rev-parse --git-common-dir)/logs/refs/stash"
	[ -f "$log" ] && grep -q " $new " "$log" && continue
	cat >&2 <<'MSG'
git stash is refused in this repository.

refs/stash is per-repository, not per-worktree. Every agent in every
worktree of this checkout pushes onto and pops off ONE stack, so two
agents stashing seconds apart exchange their entire working trees and
neither is told. That happened on 2026-08-19 and took an hour to undo.

You want a clean tree for a moment. Commit to your own branch instead:
it is yours, nobody else pops it, and you can amend or reset it freely.

  wsp commit-help    what else a shared checkout does to you
MSG
	exit 1
done
exit 0
"#
    )
}

/// What is standing at the hook path in one repository.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum State {
    /// This binary's guard, byte for byte.
    Ours,
    /// A guard wsp wrote, from a different version of this file.
    Stale,
    /// Somebody else's `reference-transaction` hook. Never overwritten.
    Foreign,
    /// Nothing there.
    Missing,
    /// `core.hooksPath` sends git somewhere outside this repository's git
    /// directory. Reported, never written to: that path may be a template
    /// shared by every repository on the machine.
    Elsewhere(PathBuf),
    /// Not a git repository, or git would not answer.
    Unknown,
}

/// The state, given what git said and what is on disk. Split out because it is
/// the whole of the judgement and needs no repository to test.
fn verdict(hooks: Option<&Path>, common: Option<&Path>, existing: Option<&str>) -> State {
    let (Some(hooks), Some(common)) = (hooks, common) else {
        return State::Unknown;
    };
    if hooks != common.join("hooks") {
        return State::Elsewhere(hooks.to_path_buf());
    }
    match existing {
        None => State::Missing,
        Some(t) if t == script() => State::Ours,
        Some(t) if t.contains("wsp stash guard") => State::Stale,
        Some(_) => State::Foreign,
    }
}

/// Ask git where hooks live and where the common git directory is.
///
/// `--git-path hooks` is one question that answers both halves that matter: it
/// resolves the common directory from inside a worktree, and it obeys
/// `core.hooksPath` — so what comes back is where git will actually look, which
/// is the only path worth writing to.
fn git(repo: &Path, args: &[&str]) -> Option<PathBuf> {
    let out = std::process::Command::new("git")
        .arg("-C")
        .arg(repo)
        .args(args)
        .env_remove("GIT_INDEX_FILE")
        .stderr(std::process::Stdio::null())
        .output()
        .ok()?;
    let text = String::from_utf8_lossy(&out.stdout).trim().to_string();
    (out.status.success() && !text.is_empty()).then(|| repo.join(text))
}

/// What is at the hook path of this repository.
pub(crate) fn state(repo: &Path) -> State {
    let hooks = git(repo, &["rev-parse", "--git-path", "hooks"]);
    let common = git(repo, &["rev-parse", "--git-common-dir"]);
    let existing = hooks
        .as_ref()
        .and_then(|h| std::fs::read_to_string(h.join("reference-transaction")).ok());
    verdict(hooks.as_deref(), common.as_deref(), existing.as_deref())
}

/// Put the guard in this repository if it is wanted and not already there.
///
/// Called where wsp puts an agent in a checkout rather than from a verb of its
/// own, for the reason [`crate::cmd_checkout::tree_for`] gives about the tree
/// itself: a step an agent has to remember is a step that gets skipped, and the
/// moment wsp knows an agent is about to work in a repository is the moment the
/// guard is owed. Silent, and it never fails anything — a spawn that could not
/// write a hook file is still a spawn, and `doctor` says the guard is missing.
pub(crate) fn ensure(repo: &Path) {
    match state(repo) {
        State::Missing | State::Stale => {}
        _ => return,
    }
    let Some(hooks) = git(repo, &["rev-parse", "--git-path", "hooks"]) else {
        return;
    };
    let path = hooks.join("reference-transaction");
    if std::fs::create_dir_all(&hooks).is_err() || std::fs::write(&path, script()).is_err() {
        return;
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let _ = std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755));
    }
}

/// Whether this git runs `reference-transaction` hooks at all. 2.28 and after.
///
/// Worth asking because the failure is silent in the worst direction: the file
/// sits there looking installed, and nothing ever reads it.
fn supported(version: &str) -> bool {
    let mut n = version
        .split_whitespace()
        .find(|w| w.starts_with(|c: char| c.is_ascii_digit()))
        .unwrap_or("")
        .split('.')
        .filter_map(|f| f.parse::<u32>().ok());
    match (n.next(), n.next()) {
        (Some(major), Some(minor)) => (major, minor) >= (2, 28),
        _ => true, // Unreadable version: say nothing rather than cry wolf.
    }
}

/// What is on the shared stack right now.
///
/// Never normal in a repository agents share: with the guard in place nothing
/// running under wsp can put an entry here, so an entry means either a person
/// at a shell or work stranded before the guard existed. Either way every agent
/// in every worktree can see it and one of them can `pop` it.
pub(crate) fn stashed(repo: &Path) -> Vec<String> {
    let Ok(out) = std::process::Command::new("git")
        .arg("-C")
        .arg(repo)
        .args(["stash", "list"])
        .env_remove("GIT_INDEX_FILE")
        .stderr(std::process::Stdio::null())
        .output()
    else {
        return Vec::new();
    };
    String::from_utf8_lossy(&out.stdout).lines().map(|l| l.to_string()).collect()
}

/// What there is to say about entries on the stack, or nothing.
///
/// One place rather than two because `doctor` and `wsp commit-help` say the
/// same thing to different readers at different moments, and prose that exists
/// twice diverges — which is the failure this whole module is about, one level
/// up. The first line stands alone; the rest are indented detail.
pub(crate) fn stash_lines(held: &[String]) -> Vec<String> {
    let Some(first) = held.first() else {
        return Vec::new();
    };
    vec![
        format!(
            "{} stash entr(y/ies) on this repository's shared stack — every worktree of it sees them, and a `pop` in any of them takes them",
            held.len()
        ),
        format!("  {first}"),
        "  `git stash show -p stash@{0}` and apply it by path where it belongs — a `pop` puts it wherever you happen to be standing".into(),
    ]
}

/// What `doctor` says about the guard, over every declared root.
///
/// `look` and `list` are passed in so the tests can state a repository's answer
/// instead of needing one, the bargain [`crate::detect_override::health`] makes
/// for the same reason.
pub(crate) fn health(
    roots: &[PathBuf],
    look: impl Fn(&Path) -> State,
    list: impl Fn(&Path) -> Vec<String>,
    version: &str,
    problems: &mut Vec<String>,
    notes: &mut Vec<String>,
) {
    let mut unguarded: Vec<String> = Vec::new();
    for root in roots {
        let shown = crate::util::contract(root);

        // A stash on the stack first, because it is the live hazard and the
        // guard's state is only ever about the next one.
        for (n, line) in stash_lines(&list(root)).into_iter().enumerate() {
            // Only the first is a sentence about this root; the rest are its
            // detail and already carry their own indent.
            problems.push(match n {
                0 => format!("{shown}: {line}"),
                _ => line,
            });
        }

        match look(root) {
            State::Ours | State::Unknown => {}
            // Folded rather than one line each, because at rollout that is
            // *every* declared root and eleven identical lines is how a reader
            // learns to skip doctor's output. It is not a fault either: a
            // repository wsp has never put an agent in does not need a guard,
            // and the next `spawn` there installs one.
            State::Missing | State::Stale => unguarded.push(shown),
            // These two stay per-root. Both are specific, both are rare, and
            // both need the path to act on.
            State::Foreign => notes.push(format!(
                "{shown}: a reference-transaction hook that is not wsp's is installed, so the stash guard is not — leave it be, and read {shown}/.git/hooks/reference-transaction"
            )),
            State::Elsewhere(path) => notes.push(format!(
                "{shown}: core.hooksPath sends git to {}, which may be shared by every repository on this machine — the stash guard is not installed there",
                crate::util::contract(&path)
            )),
        }
    }

    if !unguarded.is_empty() {
        let named: Vec<&str> = unguarded.iter().take(3).map(|s| s.as_str()).collect();
        let rest = match unguarded.len() > named.len() {
            true => format!(" and {} more", unguarded.len() - named.len()),
            false => String::new(),
        };
        notes.push(format!(
            "no stash guard in {}{rest} — a `wsp spawn` or `wsp checkout` in one installs it there",
            named.join(", ")
        ));
    }

    // Once for the run, not once per root, and only when there is a guard for
    // it to be true of.
    if !roots.is_empty() && !supported(version) {
        notes.push(format!(
            "git {} runs no reference-transaction hook, so the stash guard is a file nothing reads — 2.28 is where it starts",
            version.trim()
        ));
    }
}

/// This machine's git version, for [`health`].
pub(crate) fn git_version() -> String {
    std::process::Command::new("git")
        .arg("--version")
        .output()
        .ok()
        .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string())
        .unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn hooks(common: &Path) -> PathBuf {
        common.join("hooks")
    }

    #[test]
    fn a_repository_with_no_hook_wants_one() {
        let common = PathBuf::from("/r/.git");
        assert_eq!(verdict(Some(&hooks(&common)), Some(&common), None), State::Missing);
    }

    #[test]
    fn the_hook_this_binary_would_write_is_left_alone() {
        let common = PathBuf::from("/r/.git");
        let s = script();
        assert_eq!(verdict(Some(&hooks(&common)), Some(&common), Some(&s)), State::Ours);
    }

    #[test]
    fn an_older_wsp_guard_is_replaced_and_a_strangers_hook_is_not() {
        let common = PathBuf::from("/r/.git");
        let h = hooks(&common);
        let mine = "#!/bin/sh\n# wsp stash guard v0 — installed by wsp\nexit 0\n";
        assert_eq!(verdict(Some(&h), Some(&common), Some(mine)), State::Stale);
        let theirs = "#!/bin/sh\n# our own audit hook\nexit 0\n";
        assert_eq!(verdict(Some(&h), Some(&common), Some(theirs)), State::Foreign);
    }

    #[test]
    fn hooks_pointed_outside_the_repository_are_reported_and_never_written_to() {
        let common = PathBuf::from("/r/.git");
        let elsewhere = PathBuf::from("/etc/git-hooks");
        assert_eq!(
            verdict(Some(&elsewhere), Some(&common), None),
            State::Elsewhere(elsewhere.clone()),
            "a hooks path shared by every repository on the machine is not ours to write"
        );
    }

    #[test]
    fn a_directory_git_does_not_answer_for_is_nothing_to_say() {
        assert_eq!(verdict(None, None, None), State::Unknown);
    }

    #[test]
    fn the_hook_refuses_a_new_stash_and_allows_one_being_taken_off() {
        let s = script();
        assert!(s.contains("\"$1\" = prepared"), "two of the three phases cost one comparison");
        assert!(s.contains("*[!0]*"), "all zeroes is the last entry going, and it is allowed");
        assert!(s.contains("logs/refs/stash"), "a value already in the reflog is a pop, not a push");
        assert!(s.contains("Commit to your own branch"), "a refusal that names no alternative is an obstacle");
    }

    #[test]
    fn the_hook_names_no_wsp_because_a_missing_binary_would_block_every_git_command() {
        // The refusal message may name a wsp command — it is text on a screen
        // for a reader who has one. What must not appear is a line the shell
        // will run, because a hook that execs a missing binary refuses every
        // ref update in the repository rather than the one it was written for.
        let s = script();
        let mut in_message = false;
        for line in s.lines() {
            in_message ^= line.contains("<<'MSG'") || line.trim() == "MSG";
            if in_message || line.trim_start().starts_with('#') {
                continue;
            }
            assert!(
                !line.split_whitespace().any(|w| w == "wsp"),
                "the guard must run with no wsp on PATH: {line}"
            );
        }
    }

    #[test]
    fn git_before_the_hook_existed_is_worth_saying_out_loud() {
        assert!(supported("git version 2.35.1"));
        assert!(supported("git version 2.28.0"));
        assert!(!supported("git version 2.27.0"));
        assert!(!supported("git version 1.9.5"));
        assert!(supported("git version wobble"), "an unreadable version cries no wolf");
    }

    #[test]
    fn a_guarded_root_with_an_empty_stack_says_nothing_at_all() {
        let (mut problems, mut notes) = (Vec::new(), Vec::new());
        health(
            &[PathBuf::from("/r")],
            |_| State::Ours,
            |_| Vec::new(),
            "git version 2.35.1",
            &mut problems,
            &mut notes,
        );
        assert!(problems.is_empty() && notes.is_empty(), "{problems:?} {notes:?}");
    }

    #[test]
    fn a_stash_on_the_stack_is_a_problem_and_says_how_to_take_it_back() {
        let (mut problems, mut notes) = (Vec::new(), Vec::new());
        health(
            &[PathBuf::from("/r")],
            |_| State::Ours,
            |_| vec!["stash@{0}: WIP on master: abc1234 something".into()],
            "git version 2.35.1",
            &mut problems,
            &mut notes,
        );
        let said = problems.join("\n");
        assert!(said.contains("shared stack"), "{said}");
        assert!(said.contains("stash@{0}"), "the entry itself, not just a count: {said}");
        assert!(said.contains("by path"), "recovery is by path and never by pop: {said}");
    }

    #[test]
    fn an_empty_stack_is_silence_and_that_is_what_makes_the_check_affordable() {
        assert!(stash_lines(&[]).is_empty(), "a true sentence printed always reads as background");
    }

    #[test]
    fn an_unguarded_root_is_a_note_because_nothing_is_broken_yet() {
        let (mut problems, mut notes) = (Vec::new(), Vec::new());
        health(
            &[PathBuf::from("/r")],
            |_| State::Missing,
            |_| Vec::new(),
            "git version 2.35.1",
            &mut problems,
            &mut notes,
        );
        assert!(problems.is_empty(), "{problems:?}");
        assert!(notes.join("\n").contains("no stash guard in /r"), "{notes:?}");
    }

    #[test]
    fn every_root_being_unguarded_is_one_line_and_not_one_line_each() {
        let roots: Vec<PathBuf> = (0..11).map(|n| PathBuf::from(format!("/r{n}"))).collect();
        let (mut problems, mut notes) = (Vec::new(), Vec::new());
        health(&roots, |_| State::Missing, |_| Vec::new(), "git version 2.35.1", &mut problems, &mut notes);
        assert_eq!(notes.len(), 1, "eleven identical lines is how a reader learns to skip doctor: {notes:?}");
        assert!(notes[0].contains("and 8 more"), "{notes:?}");
    }

    #[test]
    fn a_git_too_old_for_the_hook_is_said_once_and_not_once_per_root() {
        let (mut problems, mut notes) = (Vec::new(), Vec::new());
        health(
            &[PathBuf::from("/a"), PathBuf::from("/b")],
            |_| State::Ours,
            |_| Vec::new(),
            "git version 2.20.1",
            &mut problems,
            &mut notes,
        );
        assert_eq!(notes.iter().filter(|n| n.contains("2.28")).count(), 1, "{notes:?}");
    }

    #[test]
    fn a_machine_with_no_declared_roots_is_told_nothing_about_its_git() {
        let (mut problems, mut notes) = (Vec::new(), Vec::new());
        health(&[], |_| State::Ours, |_| Vec::new(), "git version 2.20.1", &mut problems, &mut notes);
        assert!(notes.is_empty(), "{notes:?}");
    }

    // ---- end to end, against a real repository ------------------------------
    //
    // The unit tests above are about the judgement. These are about the only
    // claim that matters and the only one a Rust assertion cannot make from the
    // string alone: that git runs this file, refuses the stash, and leaves the
    // working tree exactly where it was. It was written the other way round
    // first — assume, then verify — and verifying is what found that a refused
    // `pop` corrupts the reflog.

    fn run(dir: &Path, args: &[&str]) -> std::process::Output {
        std::process::Command::new("git")
            .arg("-C")
            .arg(dir)
            .args(args)
            .env_remove("GIT_INDEX_FILE")
            .env("GIT_AUTHOR_NAME", "t")
            .env("GIT_AUTHOR_EMAIL", "t@t")
            .env("GIT_COMMITTER_NAME", "t")
            .env("GIT_COMMITTER_EMAIL", "t@t")
            .output()
            .unwrap()
    }

    fn ok(dir: &Path, args: &[&str]) {
        let out = run(dir, args);
        assert!(out.status.success(), "git {args:?}: {}", String::from_utf8_lossy(&out.stderr));
    }

    /// A repository with one commit and one dirty file, guard installed.
    fn guarded(name: &str) -> (crate::util::Isolated, PathBuf) {
        let env = crate::util::isolated(&format!("guard-{name}"));
        let dir = env.path("repo");
        std::fs::create_dir_all(&dir).unwrap();
        ok(&dir, &["init", "--quiet", "-b", "master"]);
        std::fs::write(dir.join("f.txt"), "one\n").unwrap();
        ok(&dir, &["add", "f.txt"]);
        ok(&dir, &["commit", "--quiet", "-m", "first"]);
        ensure(&dir);
        assert_eq!(state(&dir), State::Ours, "the guard did not install");
        std::fs::write(dir.join("f.txt"), "mine\n").unwrap();
        (env, dir)
    }

    #[test]
    fn git_refuses_the_stash_and_the_working_tree_does_not_move() {
        if !supported(&git_version()) {
            return;
        }
        let (_env, dir) = guarded("refuse");
        let out = run(&dir, &["stash"]);
        assert!(!out.status.success(), "the stash was taken");
        let said = String::from_utf8_lossy(&out.stderr);
        assert!(said.contains("refs/stash is per-repository"), "the reason did not reach the caller: {said}");
        assert_eq!(
            std::fs::read_to_string(dir.join("f.txt")).unwrap(),
            "mine\n",
            "a refusal that reverts the tree is the very thing being guarded against"
        );
        assert!(String::from_utf8_lossy(&run(&dir, &["stash", "list"]).stdout).trim().is_empty());
    }

    #[test]
    fn a_worktree_is_covered_by_the_install_the_trunk_got() {
        if !supported(&git_version()) {
            return;
        }
        let (_env, dir) = guarded("worktree");
        ok(&dir, &["stash", "clear"]);
        let wt = dir.join("wt");
        ok(&dir, &["worktree", "add", "--quiet", "--detach", wt.to_str().unwrap()]);
        std::fs::write(wt.join("f.txt"), "in the tree\n").unwrap();
        assert!(!run(&wt, &["stash"]).status.success(), "hooks are in the common git dir; this one was not read");
        assert_eq!(std::fs::read_to_string(wt.join("f.txt")).unwrap(), "in the tree\n");
    }

    #[test]
    fn a_stash_already_on_the_stack_can_still_be_taken_off() {
        if !supported(&git_version()) {
            return;
        }
        // Two entries, made before the guard, because `pop` with something
        // beneath it moves refs/stash to a commit rather than to zeroes — and
        // that is the case a blanket refusal would break. A refused `pop`
        // leaves the ref standing with an emptied reflog, which `git stash
        // list` reports as no stashes at all: worse than the hazard.
        let env = crate::util::isolated("guard-inherit");
        let dir = env.path("repo");
        std::fs::create_dir_all(&dir).unwrap();
        ok(&dir, &["init", "--quiet", "-b", "master"]);
        std::fs::write(dir.join("f.txt"), "one\n").unwrap();
        ok(&dir, &["add", "f.txt"]);
        ok(&dir, &["commit", "--quiet", "-m", "first"]);
        for n in ["two", "three"] {
            std::fs::write(dir.join("f.txt"), format!("{n}\n")).unwrap();
            ok(&dir, &["stash", "--quiet"]);
        }
        ensure(&dir);

        ok(&dir, &["stash", "pop"]);
        ok(&dir, &["checkout", "--quiet", "--", "f.txt"]);
        ok(&dir, &["stash", "pop"]);
        assert!(
            String::from_utf8_lossy(&run(&dir, &["stash", "list"]).stdout).trim().is_empty(),
            "the stack did not empty"
        );
        assert!(
            run(&dir, &["rev-parse", "--verify", "--quiet", "refs/stash"]).stdout.is_empty(),
            "refs/stash outlived its reflog — the half-state a refused pop leaves"
        );
    }

    #[test]
    fn ordinary_git_carries_on_working_with_the_guard_in_place() {
        if !supported(&git_version()) {
            return;
        }
        let (_env, dir) = guarded("ordinary");
        ok(&dir, &["commit", "--quiet", "-am", "second"]);
        ok(&dir, &["branch", "side"]);
        ok(&dir, &["tag", "v1"]);
        ok(&dir, &["checkout", "--quiet", "side"]);
        // The one that would bite `wsp land`: rebase's autostash keeps its
        // commit out of refs/stash entirely, and this proves it rather than
        // trusting the documentation.
        std::fs::write(dir.join("f.txt"), "dirty\n").unwrap();
        ok(&dir, &["rebase", "--autostash", "master"]);
        assert_eq!(std::fs::read_to_string(dir.join("f.txt")).unwrap(), "dirty\n");
    }

    #[test]
    fn a_second_install_over_our_own_hook_changes_nothing() {
        if !supported(&git_version()) {
            return;
        }
        let (_env, dir) = guarded("idempotent");
        let path = dir.join(".git/hooks/reference-transaction");
        let before = std::fs::metadata(&path).unwrap().modified().unwrap();
        ensure(&dir);
        assert_eq!(std::fs::metadata(&path).unwrap().modified().unwrap(), before, "rewritten for nothing");
    }

    #[test]
    fn a_hook_somebody_else_wrote_is_never_overwritten() {
        let env = crate::util::isolated("guard-foreign");
        let dir = env.path("repo");
        std::fs::create_dir_all(dir.join(".git/hooks")).unwrap();
        ok(&dir, &["init", "--quiet", "-b", "master"]);
        let path = dir.join(".git/hooks/reference-transaction");
        std::fs::write(&path, "#!/bin/sh\n# theirs\nexit 0\n").unwrap();
        ensure(&dir);
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "#!/bin/sh\n# theirs\nexit 0\n");
        assert_eq!(state(&dir), State::Foreign);
    }
}
