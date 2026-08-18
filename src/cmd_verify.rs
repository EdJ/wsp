//! `wsp verify` — build and test this agent's change in a tree of its own.
//!
//! A build in a shared checkout tells you nothing. On 2026-08-16, with four
//! agents in this repository, `cargo check` failed on another agent's
//! half-added `Row::Group` and `cargo test` failed three storyboard tests from
//! a third agent's uncommitted work. A green build there does not mean your
//! change is good and a red one does not mean it is bad — so the isolation
//! worktree in step 3 of `wsp commit-help` is not an extra check, it is the
//! only build whose result means anything.
//!
//! Every agent already does this by hand. They do it differently, they forget
//! parts of it, and the parts they forget are the same three every time:
//!
//! - `env -u GIT_INDEX_FILE` on `git worktree add`, which otherwise writes the
//!   new worktree's index *over* the private one you just built — and leaves
//!   `git apply` refusing an empty patch, which reads like a staging mistake
//!   rather than what it is.
//! - `git read-tree HEAD` *immediately* before the commit rather than minutes
//!   before, so a commit does not land as a silent revert of whatever arrived
//!   in the parent during the build.
//! - The same `env -u` on the `read-tree` that puts the shared index back.
//!
//! This module can only fix the first, because it never commits. It fixes it
//! by construction rather than by remembering: `GIT_INDEX_FILE` is set per
//! command, on the two commands that want it, and stripped from every other —
//! so there is no exported variable for `worktree add` to pick up, whether or
//! not the caller is halfway through the commit procedure with one set.
//!
//! # The scratch tree belongs to the checkout, not to the agent
//!
//! The standing rule from 2026-08-15 was a detached worktree per build, keyed
//! on the workspace and kept, with `CARGO_TARGET_DIR` beside it — one tree per
//! agent to leak rather than one per commit. That argument assumed the agent
//! had nowhere else of its own. Since [`crate::cmd_checkout`] it has: a pane
//! opened on a task stands in `<trunk>/.worktrees/<task>`, where nobody else's
//! half-finished work can appear.
//!
//! Keyed on the workspace, the tree then leaked in the one direction the
//! keying was supposed to stop. A workspace is stable while it exists, which is
//! what made it the right key — but it lasts only as long as the agent in it,
//! and every `wsp spawn` opens a new one. So "one agent, many tasks, one warm
//! tree" was never what happened: measured 2026-08-17,
//! `~/.local/state/wsp/build` held **9.6G in 30 trees**, one per agent that had
//! ever run this command, every one of them cold on its first build and none of
//! them ever removed. A cleanup step nobody runs is the lesson of every leak on
//! t-260815-022, and this was one more.
//!
//! So inside a checkout the scratch goes *under the checkout*, at
//! `target/wsp-verify`, and takes its target directory with it. It dies with
//! the tree it belongs to: `wsp checkout --rm` and `--sweep` already remove the
//! whole directory, so there is no second thing to remember and no second thing
//! to leak. No task, no tree — the rule `checkout` states, now true of the
//! build as well. What was unbounded is now one per live task.
//!
//! # Why it does *not* share the checkout's `target/`, which is the obvious idea
//!
//! Because the warmth lives in the target directory and not in the tree, the
//! obvious saving is to point both trees at one `CARGO_TARGET_DIR` — the
//! checkout's own — so that an agent's `cargo test` and this command warm each
//! other and the 295M is not duplicated. It was measured on 2026-08-17 before
//! being believed, and it has to be thrown away.
//!
//! The encouraging half is real: two trees against one target directory do not
//! thrash. The second builds in **0.04s** where the first took 9.7s, and
//! alternating rebuilds only what genuinely differs — `cargo test` 13s against
//! 39s cold.
//!
//! The disqualifying half is that cargo records the dependencies of a unit as
//! *absolute paths*, in `target/debug/.fingerprint/<unit>/dep-*`, and judges
//! freshness by their mtimes. Build the scratch tree and that file comes to
//! read `…/target/wsp-verify/tree/src/cmd_verify.rs`. The next `cargo test` in
//! the **checkout** then asks whether the scratch tree has changed, gets no for
//! an answer, prints `Fresh wsp v0.1.0`, and reruns the old binary. Observed
//! here, not reasoned about: this module was edited, `cargo test` reported 482
//! passing, and the test that had just been added was not among them — the
//! compiled binary still held the previous run's test names.
//!
//! That is a green build for source that was never compiled, in the agent's own
//! tree, arriving silently. It is the exact failure the top of this file
//! describes, reintroduced by the fix for it, and it costs more than the 295M
//! it saves. So the trees get a target directory each, and the saving here is
//! from *bounding* the number of them rather than from sharing one.
//!
//! # A red run is the only run that knows what was red
//!
//! Until 2026-08-18 a failing test run printed this and nothing else:
//!
//! ```text
//! error: test failed, to rerun pass `--bin wsp`
//! ✗ cargo test failed in 25s
//! ```
//!
//! The name was never in it. That is expensive rather than untidy because this
//! suite fails intermittently — five red-then-greens on unmodified trees, the
//! last two at load average ~10 — so the sequence is always: verify goes red,
//! you re-run to find out what broke, the re-run is green, and the name is
//! gone. It cost the name twice in one day. You cannot ask a green run what was
//! red, and `robustness-068` cannot progress without the names.
//!
//! So a failing run keeps three things it used to throw away:
//!
//! - The test path and what its assertion said, parsed out of libtest's own
//!   report and printed by this command rather than left to scrollback.
//! - The whole cargo output, on disk beside `patch.diff`, with the path in the
//!   error line. The tree is already left standing on failure; the run that
//!   produced it is the only one that can be kept.
//! - One re-run of the named test *alone*, and whether it passed there. That
//!   single line — "failed in the suite, passed alone" — is the signature of a
//!   shared-state flake, and taking it on every occurrence is the difference
//!   between a measurement and somebody happening to be watching.
//!
//! What it deliberately does not do is re-run the *suite* and report green. A
//! flake that is automatically swallowed stops being observable, and this one is
//! the most interesting unexplained thing in the repository. The exit status is
//! decided by the run that failed; the re-run alone is evidence printed beside
//! it, never a second opinion that overrules it.
//!
//! # The trees are separate; the machine is not
//!
//! A tree per task means a *cold* build per task, and each `cargo` takes `-j8`
//! as though it owned the laptop — load 468 and twenty-one `rustc` on
//! 2026-08-17. So the tree this builds in is no longer always the private one
//! above: [`crate::sharing`] lends it one of a few warm trees kept per
//! repository, exclusively, and hands it a share of the cores. Measured on this
//! command, 40s cold against 19s warm.
//!
//! The isolation is untouched, which is the property that made the warm tree
//! the shareable thing and the target directory not: what arrives is a tree
//! reset to *this* agent's HEAD with *this* agent's patch on it, with nobody
//! else in it while the build runs. Only the tree and its target move; the
//! private index, the patch and the failing run's log stay in this agent's own
//! directory, where two agents cannot overwrite each other's.
//!
//! # Outside a checkout, nothing changes
//!
//! The trunk is still shared — the coordination seat stands there, and so does
//! any bare shell — so an agent building there still gets a tree of its own
//! under `WSP_STATE`, keyed on the workspace, exactly as before. That is where
//! the original argument still holds, and it is the only place left where it
//! does.

use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::Instant;

use serde_json::json;

use crate::sharing::{self, Share};
use crate::store::Store;
use crate::util;
use crate::Args;

/// Run git somewhere and take its stdout, or `None` if it failed.
///
/// `GIT_INDEX_FILE` is stripped on the way in, always. Two different callers
/// would otherwise be wrong in two different ways: an agent partway through
/// `wsp commit-help` has one exported and every read here would inspect its
/// staging rather than the repository, and `worktree add` would write the new
/// worktree's index over it. The two commands that genuinely want a private
/// index set it themselves, on themselves.
pub(crate) fn git(dir: &Path, args: &[&str]) -> Option<String> {
    let out = Command::new("git")
        .arg("-C")
        .arg(dir)
        .args(args)
        .env_remove("GIT_INDEX_FILE")
        .stderr(Stdio::null())
        .output()
        .ok()?;
    out.status.success().then(|| String::from_utf8_lossy(&out.stdout).into_owned())
}

/// The same, when the output is not wanted but the failure is. Returns git's
/// stderr on failure, because "what did git say" is the whole diagnostic.
pub(crate) fn git_ok(dir: &Path, args: &[&str]) -> Result<(), String> {
    let out = Command::new("git")
        .arg("-C")
        .arg(dir)
        .args(args)
        .env_remove("GIT_INDEX_FILE")
        .output()
        .map_err(|e| format!("git: {e}"))?;
    if out.status.success() {
        return Ok(());
    }
    let msg = String::from_utf8_lossy(&out.stderr).trim().to_string();
    Err(if msg.is_empty() { format!("git {} failed", args.join(" ")) } else { msg })
}

/// The repository containing `dir`, resolved through git rather than by walking
/// up looking for `.git` — a worktree's `.git` is a file, and a submodule's is
/// somewhere else again.
pub(crate) fn toplevel(dir: &Path) -> Option<PathBuf> {
    let out = git(dir, &["rev-parse", "--show-toplevel"])?;
    let line = out.trim();
    (!line.is_empty()).then(|| PathBuf::from(line))
}

/// Which agent is asking.
///
/// The workspace, not the pane. Pane ids are reissued across herdr restarts —
/// which is why claims are keyed on the workspace too — and a build tree that
/// changed identity on every restart would be a cold build on every restart.
/// Outside herdr there is no workspace, and `solo` is honest: one shell at a
/// terminal gets one tree and shares it with the next.
/// `wsp sandbox` keys its instance the same way, deliberately: an agent's build
/// tree and its sandbox are the same pair of scratch things, and one name for
/// both means `verify` and `sandbox` cannot disagree about whose they are.
pub fn agent_key() -> String {
    if let Ok(v) = std::env::var("WSP_AGENT") {
        if !v.trim().is_empty() {
            return util::slugify(v.trim());
        }
    }
    match crate::herdr::Env::read().workspace_id {
        Some(ws) if !ws.trim().is_empty() => util::slugify(ws.trim()),
        _ => "solo".to_string(),
    }
}

/// Where an agent standing in the *trunk* keeps its build tree: under the state
/// directory rather than the store, because it is machine-local, it is large,
/// and it is not worth committing. Under `WSP_STATE` rather than a fixed path
/// so a sandbox (see t-260816-056) gets its own and does not warm — or corrupt
/// — the real one.
///
/// Named for the repository and keyed on the agent. `repo` has to be the trunk
/// for the name to mean anything, which is why [`scratch`] resolves it rather
/// than passing whatever tree the caller is standing in.
pub(crate) fn build_dir(store: &Store, repo: &Path, key: &str) -> PathBuf {
    let name = repo.file_name().and_then(|s| s.to_str()).unwrap_or("repo");
    store.state.join("build").join(format!("{}-{}", util::slugify(name), key))
}

/// Remove the build trees under the state directory that no live workspace
/// owns, and say which went.
///
/// The residue of keying on the workspace. Those trees outlived the workspaces
/// that named them, so no agent alive can name one to remove it — 9.6G in 30 of
/// them on 2026-08-17 — and the arrangement that made them is gone.
///
/// `live` is the workspace ids herdr reported, and `None` is herdr not
/// answering. That distinction is the whole guard, and it is the same one
/// [`crate::cmd_agent::may_reap`] makes for the same reason: silence is not
/// evidence that the work stopped. A herdr that is down, or slow, reports
/// nothing, which looks exactly like a machine with no agents on it — and this
/// would then delete the tree every running agent is mid-build in. So `None`
/// removes nothing at all, and an empty list is only believed when herdr said
/// it.
///
/// Passed in rather than read here so the judgement can be tested without a
/// live herdr, which is the one thing that would make it untestable and it is
/// the only thing here worth testing.
///
/// Directories only. A stale `git worktree` registration left behind is pruned
/// by [`ensure_tree`] or by `checkout` the next time either touches the
/// repository, and pruning it here would mean guessing which repository each
/// tree came from.
fn clear_build_dirs(store: &Store, live: Option<&[String]>, mine: &Path) -> Vec<String> {
    let Some(live) = live else { return Vec::new() };
    let root = store.state.join("build");
    let Ok(entries) = std::fs::read_dir(&root) else { return Vec::new() };
    let mut gone = Vec::new();
    for e in entries.flatten() {
        let path = e.path();
        if !path.is_dir() || path == mine {
            continue;
        }
        let Some(name) = path.file_name().and_then(|s| s.to_str()) else { continue };
        // `<repo>-<workspace>`, and the repository name can hold a dash, so the
        // key is the tail rather than the second field.
        if live.iter().any(|ws| name.ends_with(&format!("-{}", util::slugify(ws)))) {
            continue;
        }
        if std::fs::remove_dir_all(&path).is_ok() {
            gone.push(util::contract(&path));
        }
    }
    gone.sort();
    gone
}

/// Everything one run of `verify` needs a place for.
///
/// Resolved once and passed around, because the two arrangements differ in more
/// than one path and a caller that worked out three of them separately would be
/// able to get two right and one wrong. `wsp install` looks for the release
/// build this produced, and used to compute the directory itself — from the
/// tree it was standing in rather than the trunk, so inside a checkout it
/// looked somewhere `verify` had never written.
pub(crate) struct Scratch {
    /// Holds the private index, the patch, and `tree`. What `--rm` removes.
    pub dir: PathBuf,
    /// The source tree that gets built: HEAD, reset and re-patched every run.
    pub tree: PathBuf,
    /// `CARGO_TARGET_DIR`, always inside `dir` and never shared with the tree
    /// the agent edits in — see the header for the measurement that settled
    /// that, and what sharing it silently did to `cargo test`.
    pub target: PathBuf,
    /// The checkout it belongs to, or `None` for the trunk. The caller wants
    /// the distinction to say which arrangement it is looking at.
    pub checkout: Option<PathBuf>,
}

/// Which of the two arrangements applies, and where each part goes.
///
/// The test is the path rule and not a git question: a per-task checkout is
/// exactly a tree whose own root is the `<trunk>/.worktrees/<task>` that
/// [`crate::cmd_checkout::worktree_of`] names. Asking git which worktree owns a
/// path costs a process, and the layout is ours to define, so the answer is
/// already in the path — the same bargain `overlap` makes for the same reason.
pub(crate) fn scratch(store: &Store, repo: &Path, key: &str) -> Scratch {
    if crate::cmd_checkout::worktree_of(repo).as_deref() == Some(repo) {
        // Under `target/` rather than a dotted directory of its own: it is
        // build output, it is already gitignored in every cargo project, and
        // putting it there keeps `git status` in the checkout clean — which
        // matters more than tidiness, because `checkout --sweep` refuses to
        // remove a tree with anything uncommitted in it, and a scratch
        // directory git could see would make every tree permanently unsweepable.
        let dir = repo.join("target").join("wsp-verify");
        return Scratch {
            tree: dir.join("tree"),
            target: dir.join("target"),
            dir,
            checkout: Some(repo.to_path_buf()),
        };
    }
    let named_for = crate::cmd_checkout::trunk(repo).unwrap_or_else(|| repo.to_path_buf());
    let dir = build_dir(store, &named_for, key);
    Scratch { tree: dir.join("tree"), target: dir.join("target"), dir, checkout: None }
}

/// The file naming where the last build here actually put its artefacts.
///
/// One line, in the agent's own scratch directory. It exists because the answer
/// stopped being derivable: a build now lands in whichever warm tree was free,
/// and `wsp install` — which looks for the release binary this command produced
/// — cannot work that out from the checkout it is standing in.
pub(crate) const BUILT_AT: &str = "built-at";

/// What the last build here wrote down, or `None` if none has — or if it names
/// a tree that has since gone, which `--rm` makes ordinary rather than exotic.
///
/// Named rather than read twice, because `install` asks the same question for a
/// second reason: whether it may name a path in "nothing built to install". A
/// build lands in whichever warm tree was free, so the only path anybody can
/// honestly name is the one a build wrote down.
pub(crate) fn built_at(dir: &Path) -> Option<PathBuf> {
    std::fs::read_to_string(dir.join(BUILT_AT))
        .ok()
        .map(|s| PathBuf::from(s.trim()))
        .filter(|p| p.is_dir())
}

/// Where the last build here put its target directory, for a caller that wants
/// the artefacts rather than a place to build.
///
/// Falls back to [`scratch`] when there is no pointer or it names something
/// gone, which is both the pre-warm-tree arrangement and the honest answer for
/// a checkout that has never run a build.
pub(crate) fn last_build(store: &Store, repo: &Path, key: &str) -> Scratch {
    let sc = scratch(store, repo, key);
    match built_at(&sc.dir) {
        Some(target) => {
            let tree = target.parent().map_or_else(|| sc.tree.clone(), |d| d.join("tree"));
            Scratch { tree, target, ..sc }
        }
        None => sc,
    }
}

/// Remove warm trees: the one this agent last built in, or with `--all` every
/// one for this repository that nobody is building in.
///
/// The default is the narrow one deliberately. `--rm` means "the tree I have
/// been building in has gone wrong", and after this change that tree is shared
/// — so throwing away all three on the way past would hand every other agent a
/// cold build to fix one agent's problem. Which one it was is not a guess: the
/// build wrote it down.
///
/// A tree somebody is building in is never removed, and the git worktree
/// registration goes with the directory. A directory removed without it makes
/// the next `worktree add` refuse, with a message that reads like a bug in this
/// command rather than a stale registration.
fn clear_warm(store: &Store, repo: &Path, named_for: &Path, dir: &Path, all: bool) -> usize {
    let mine = std::fs::read_to_string(dir.join(BUILT_AT))
        .ok()
        .map(|s| PathBuf::from(s.trim()))
        .and_then(|target| target.parent().map(Path::to_path_buf))
        .filter(|d| d.starts_with(store.state.join("warm")))
        .and_then(|d| crate::sharing::warm_named(&d));
    let claimed = if all {
        crate::sharing::warm_each(&store.state, named_for, crate::sharing::WARM_TREES)
    } else {
        mine.into_iter().collect()
    };
    let mut gone = 0;
    for w in claimed {
        if !w.dir.exists() {
            continue;
        }
        let _ = git(repo, &["worktree", "remove", "--force", &w.tree().display().to_string()]);
        if std::fs::remove_dir_all(&w.dir).is_ok() {
            gone += 1;
        }
        let _ = git(repo, &["worktree", "prune"]);
    }
    gone
}

/// The paths whose change is under test, as `git add` pathspecs.
///
/// Naming them is the point — "apply the diff for the paths you name, and
/// nothing else" — but requiring them on every call would make the command
/// something people skip. So the default is everything changed, and the file
/// list is printed either way, prominently, because that is step 2 of the
/// commit procedure: a wrong file list is the tell, and the hunks inside one
/// all look plausible because they are somebody's real work.
fn staged_files(index: &Path, repo: &Path) -> Vec<String> {
    let out = Command::new("git")
        .arg("-C")
        .arg(repo)
        .args(["diff", "--cached", "--name-only"])
        .env("GIT_INDEX_FILE", index)
        .stderr(Stdio::null())
        .output()
        .ok();
    out.map(|o| {
        String::from_utf8_lossy(&o.stdout)
            .lines()
            .map(|s| s.trim().to_string())
            .filter(|s| !s.is_empty())
            .collect()
    })
    .unwrap_or_default()
}

/// Everything the repository has that HEAD does not, named or not.
///
/// The counterweight to naming paths. Naming them keeps another agent's work
/// out of your build, which is the point — but it also lets you leave out
/// something of *your own*, and that fails in the worst direction: a patch
/// holding a new module that nothing declares compiles perfectly, because
/// nothing compiles it. Measured while writing this, `wsp verify
/// src/cmd_verify.rs` went green in 7s against a change that did not build.
///
/// So what was left out is printed beside what was taken. In a shared tree
/// most of it will be somebody else's and correctly excluded; the whole value
/// is that you can see at a glance whether one of them is yours.
fn changed_files(repo: &Path) -> Vec<String> {
    let Some(out) = git(repo, &["status", "--porcelain", "--untracked-files=all"]) else {
        return Vec::new();
    };
    out.lines()
        .filter_map(|l| l.get(3..))
        // A rename is written `old -> new`; the new name is the one on disk.
        .map(|p| p.rsplit(" -> ").next().unwrap_or(p).trim().trim_matches('"').to_string())
        .filter(|p| !p.is_empty())
        .collect()
}

/// Build the patch under test, through a private index at HEAD.
///
/// The same three commands as steps 1 and 3 of `wsp commit-help`, in the same
/// order, which is deliberate: what this builds is exactly what that procedure
/// would stage, so a green `verify` is a statement about the commit you are
/// about to make rather than about some neighbouring tree.
///
/// `git add` rather than `git diff HEAD` because `add` sees files that are not
/// tracked yet. A new module is the commonest thing an agent adds and the
/// easiest thing for a diff-based patch to miss — and missing it fails in the
/// worst direction, with a green build for a change that does not compile.
/// `scratch` is this command's own working directory — the private index, the
/// patch, the build tree. Inside a checkout it is under the tree by design, and
/// a sandbox pointed at a state directory in the tree puts it there by accident;
/// either way `git add -A` would otherwise stage verify's own index into the
/// patch it is building, which is a change that then fails to apply against HEAD
/// for reasons no one could read.
///
/// The exclusion is asked for only when git cannot already see it, and that is
/// not an optimisation. An `:(exclude)` pathspec under an ignored directory
/// reads to `git add` as an ignored path named on purpose, so it refuses the
/// whole command — `error: the following paths are ignored ... target`. The
/// checkout arrangement puts the scratch under `target/`, which every cargo
/// project ignores, so the guard against staging it is exactly what made it
/// impossible to stage anything.
fn build_patch(
    repo: &Path,
    cwd: &Path,
    index: &Path,
    scratch: &Path,
    paths: &[String],
) -> Result<String, String> {
    let _ = std::fs::remove_file(index);
    let run = |dir: &Path, args: &[&str]| -> Result<std::process::Output, String> {
        Command::new("git")
            .arg("-C")
            .arg(dir)
            .args(args)
            .env("GIT_INDEX_FILE", index)
            .output()
            .map_err(|e| format!("git: {e}"))
    };
    let check = |out: std::process::Output, what: &str| -> Result<Vec<u8>, String> {
        if out.status.success() {
            return Ok(out.stdout);
        }
        let msg = String::from_utf8_lossy(&out.stderr).trim().to_string();
        Err(if msg.is_empty() { format!("{what} failed") } else { msg })
    };

    check(run(repo, &["read-tree", "HEAD"])?, "read-tree")?;

    // Pathspecs are resolved from where they were typed, so the named case runs
    // in the caller's directory rather than at the repository root.
    if paths.is_empty() {
        let mut argv: Vec<String> = vec!["add".into(), "-A".into(), "--".into(), ".".into()];
        if let Ok(rel) = scratch.strip_prefix(repo) {
            let rel = rel.display().to_string();
            // `check-ignore` exits 0 when the path is ignored, which is `git`
            // returning `Some` here.
            if git(repo, &["check-ignore", "-q", "--", &rel]).is_none() {
                argv.push(format!(":(exclude){rel}"));
            }
        }
        let argv: Vec<&str> = argv.iter().map(|s| s.as_str()).collect();
        check(run(repo, &argv)?, "add")?;
    } else {
        let mut argv: Vec<&str> = vec!["add", "--"];
        argv.extend(paths.iter().map(|s| s.as_str()));
        check(run(cwd, &argv)?, "add")?;
    }

    let patch = check(run(repo, &["diff", "--cached", "--binary"])?, "diff")?;
    Ok(String::from_utf8_lossy(&patch).into_owned())
}

/// Make this agent's worktree if it has none, and put it back to `head` if it
/// has. `--detach` because the tree holds one agent's patch and never a branch:
/// there is nothing here to push, and a branch would be a second thing to keep
/// true.
fn ensure_tree(repo: &Path, tree: &Path, head: &str) -> Result<bool, String> {
    if tree.join(".git").exists() {
        // `reset` puts tracked files back; `clean` removes what the last run's
        // patch added, which reset leaves behind as untracked. Both are safe
        // here in a way they would never be in a shared checkout — nothing in
        // this tree is anybody's working copy.
        git_ok(tree, &["reset", "--hard", head, "--quiet"])?;
        git_ok(tree, &["clean", "-fdq"])?;
        return Ok(false);
    }
    // A directory left behind by a removed worktree, or a worktree registration
    // whose directory is gone: both make `add` refuse, and both are the leak
    // this command exists to stop leaving.
    let _ = std::fs::remove_dir_all(tree);
    let _ = git(repo, &["worktree", "prune"]);
    std::fs::create_dir_all(tree.parent().unwrap_or(tree)).map_err(|e| e.to_string())?;
    git_ok(repo, &["worktree", "add", "--detach", "--quiet", &tree.display().to_string(), head])?;
    Ok(true)
}

/// One cargo run: echoed as it arrives, and kept.
///
/// It is echoed because the compiler's own output is the thing being asked for
/// — a summary of a build failure is worth nothing next to the error — and
/// `echo` is off only for `--json`, where the caller wants one object rather
/// than a build log with an object at the end of it.
///
/// It is kept unconditionally, which it was not before: the run that fails is
/// the only run that knows what failed, and on this suite the next one is
/// usually green. Nothing here decides that a run is interesting; by the time
/// anything could, the output would be gone.
///
/// Two threads rather than one [`Command::output`] call, which would be
/// simpler and would print nothing until cargo exited — for a 25s test run
/// that is the difference between watching a build and watching a cursor.
fn cargo(tree: &Path, target: &Path, argv: &[&str], echo: bool, share: &Share) -> (bool, String) {
    let mut cmd = Command::new("cargo");
    cmd.args(argv)
        .current_dir(tree)
        .env("CARGO_TARGET_DIR", target)
        .env_remove("GIT_INDEX_FILE")
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    // The one thing this build does not decide for itself: how much of the
    // machine it may have, and where the compiler cache is. See
    // [`crate::sharing`] — the target directory above is still its own, and the
    // cache is keyed on inputs rather than on paths, so neither can make this
    // build tell the truth about somebody else's tree.
    share.apply(&mut cmd);
    let mut child = match cmd.spawn() {
        Ok(c) => c,
        Err(e) => {
            if echo {
                eprintln!("wsp: cargo: {e}");
            }
            return (false, format!("cargo: {e}"));
        }
    };
    // Both streams are drained concurrently because a pipe nobody reads fills
    // and stops the writer: reading stdout to the end first would deadlock the
    // moment cargo wrote more warnings than a pipe buffer holds.
    let out = child.stdout.take().map(|s| std::thread::spawn(move || pump(s, echo, false)));
    let err = child.stderr.take().map(|s| std::thread::spawn(move || pump(s, echo, true)));
    let ok = child.wait().map(|st| st.success()).unwrap_or(false);
    let mut text = out.and_then(|t| t.join().ok()).unwrap_or_default();
    text.push_str(&err.and_then(|t| t.join().ok()).unwrap_or_default());
    (ok, text)
}

/// Read one of cargo's streams to the end, passing it on as it arrives.
fn pump(mut r: impl std::io::Read, echo: bool, is_err: bool) -> String {
    use std::io::Write;
    let mut kept: Vec<u8> = Vec::new();
    let mut buf = [0u8; 8192];
    loop {
        match r.read(&mut buf) {
            Ok(0) | Err(_) => break,
            Ok(n) => {
                kept.extend_from_slice(&buf[..n]);
                if echo {
                    // Flushed per chunk: cargo's progress is only progress if
                    // it arrives while the build is still running.
                    if is_err {
                        let mut h = std::io::stderr();
                        let _ = h.write_all(&buf[..n]);
                        let _ = h.flush();
                    } else {
                        let mut h = std::io::stdout();
                        let _ = h.write_all(&buf[..n]);
                        let _ = h.flush();
                    }
                }
            }
        }
    }
    String::from_utf8_lossy(&kept).into_owned()
}

/// One test the run reported as failed: where it panicked, and what it said.
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct Failure {
    pub name: String,
    pub at: Option<String>,
    pub message: Option<String>,
}

/// The failing tests, out of a cargo test run's own report.
///
/// Two things in libtest's output name a failure and neither is enough alone.
/// The trailing `failures:` list is the authoritative set — every failed test
/// is in it, in one place, whatever the format — but it carries no message. The
/// `---- <name> stdout ----` blocks above it carry the panic and the assertion
/// but are the part a harness is free to shorten. So the list decides who
/// failed, the blocks say what they said, and a name in the list with no block
/// is still reported by name.
///
/// A compile failure names nothing here, deliberately. Cargo has already
/// printed the error, in full, and inventing a test name for it would be worse
/// than the silence this replaces.
fn failures(output: &str) -> Vec<Failure> {
    let lines: Vec<&str> = output.lines().collect();

    let mut detail: Vec<Failure> = Vec::new();
    let mut i = 0;
    while i < lines.len() {
        let head = lines[i].trim();
        let Some(name) = head.strip_prefix("---- ").and_then(|r| r.strip_suffix(" stdout ----"))
        else {
            i += 1;
            continue;
        };
        let mut block: Vec<&str> = Vec::new();
        let mut j = i + 1;
        while j < lines.len() {
            let l = lines[j].trim();
            if l.starts_with("---- ") || l == "failures:" || l.starts_with("test result:") {
                break;
            }
            block.push(lines[j]);
            j += 1;
        }
        detail.push(Failure {
            name: name.trim().to_string(),
            at: panic_site(&block),
            message: assertion(&block),
        });
        i = j;
    }

    let mut names: Vec<String> = Vec::new();
    let mut k = 0;
    while k < lines.len() {
        if lines[k].trim() != "failures:" {
            k += 1;
            continue;
        }
        k += 1;
        // The detail section also opens with `failures:`, and what follows it
        // is a blank line and an unindented `----` header — so the indent test
        // ends that one immediately and only the real list is collected.
        while k < lines.len() && lines[k].starts_with("    ") && !lines[k].trim().is_empty() {
            let n = lines[k].trim().to_string();
            if !names.contains(&n) {
                names.push(n);
            }
            k += 1;
        }
    }

    if names.is_empty() {
        return detail;
    }
    names
        .into_iter()
        .map(|n| match detail.iter().find(|f| f.name == n) {
            Some(f) => f.clone(),
            None => Failure { name: n, at: None, message: None },
        })
        .collect()
}

/// `src/panel/keys.rs:412:9`, out of the panic line.
fn panic_site(block: &[&str]) -> Option<String> {
    let line = block.iter().find(|l| l.contains("panicked at "))?;
    let rest = line.split("panicked at ").nth(1)?.trim().trim_end_matches(':');
    (!rest.is_empty()).then(|| rest.to_string())
}

/// What the assertion said — the line under the panic, or failing that the
/// first thing the test printed, which is where a `Result`-returning test's
/// error ends up.
fn assertion(block: &[&str]) -> Option<String> {
    let after = block.iter().position(|l| l.contains("panicked at ")).map(|p| p + 1).unwrap_or(0);
    block[after.min(block.len())..]
        .iter()
        .map(|l| l.trim())
        .find(|l| !l.is_empty() && !l.starts_with("note: "))
        .map(|l| l.to_string())
}

/// Run one named test by itself, once, and say whether it passed there.
///
/// `Some(true)` is the whole signature of a shared-state flake and the
/// measurement `robustness-068` wants on every occurrence. It is evidence
/// printed beside the failure and never a verdict: the suite failed, and the
/// exit status is still the suite's.
///
/// `None` is "not measured", which is not the same as passing and is printed as
/// nothing at all rather than as a green word.
fn rerun_alone(tree: &Path, target: &Path, name: &str, share: &Share) -> Option<bool> {
    // A doc test is named `src/store.rs - Store::at (line 12)`, which is not
    // something libtest can be handed as an exact filter. Anything with a space
    // in it is left alone rather than guessed at.
    if name.is_empty() || name.split_whitespace().count() != 1 {
        return None;
    }
    let (ok, text) = cargo(tree, target, &["test", "--quiet", "--", "--exact", name], false, share);
    if !ok {
        return Some(false);
    }
    // A filter that matched nothing exits green. Reporting that as "passed
    // alone" would invent the measurement instead of taking it, so the pass has
    // to be one test actually having run.
    text.contains("1 passed; 0 failed").then_some(true)
}

pub fn verify(store: &Store, args: &Args) -> i32 {
    let p = util::Paint::new();
    let json_out = args.json();

    let Ok(cwd) = std::env::current_dir() else {
        eprintln!("wsp: cannot read the current directory");
        return 2;
    };
    let Some(repo) = toplevel(&cwd) else {
        eprintln!("wsp: {} is not in a git repository", util::contract(&cwd));
        return 2;
    };

    let key = agent_key();
    // `repo` goes on being the tree under your hands, which is where the patch
    // comes from and where the worktree is added; `scratch` decides where the
    // build goes from what kind of tree that is.
    let Scratch { dir, tree, target, checkout } = scratch(store, &repo, &key);
    // The warm pool is keyed on the repository, and inside a checkout `repo` is
    // named for the task rather than the repository — `.worktrees/robustness-070`
    // — so the trunk is what names it. Otherwise every task would have a pool of
    // its own, which is the cold build this is trying to stop having.
    let named_for = crate::cmd_checkout::trunk(&repo).unwrap_or_else(|| repo.clone());

    // `--rm` before anything else: the point of it is a tree you can drop when
    // it has gone wrong, and needing a working repository to drop it would be
    // exactly backwards.
    if args.has("rm") {
        let existed = tree.exists();
        // The warm tree first, because which one it was is written down *in*
        // the directory the next line removes.
        let warm = clear_warm(store, &repo, &named_for, &dir, args.has("all"));
        let _ = git(&repo, &["worktree", "remove", "--force", &tree.display().to_string()]);
        let _ = std::fs::remove_dir_all(&dir);
        let _ = git(&repo, &["worktree", "prune"]);
        // `--all` is for the residue rather than for this run: the trees the
        // per-workspace keying left behind, 9.6G on this machine and reachable
        // by no other command, since the workspaces that owned them are gone.
        // Only under the state directory, and never a checkout's — a checkout's
        // belongs to a tree somebody may be standing in, and goes when that
        // does.
        let residue = if args.has("all") {
            let live: Option<Vec<String>> =
                crate::herdr::workspaces().ok().map(|w| w.into_iter().map(|x| x.id).collect());
            clear_build_dirs(store, live.as_deref(), &dir)
        } else {
            Vec::new()
        };
        if json_out {
            println!(
                "{}",
                json!({
                    "removed": existed,
                    "path": util::contract(&dir),
                    "also": residue.len(),
                    "warm": warm,
                })
            );
        } else {
            if existed {
                println!("removed {}", util::contract(&dir));
            } else {
                // Named by what owns it, which is the checkout inside one and
                // the agent outside — otherwise "no build tree for w20" reads
                // as a claim about the agent in the one arrangement where the
                // agent is not what the tree is keyed on.
                match &checkout {
                    Some(c) => println!("no build tree in {}", util::contract(c)),
                    None => println!("no build tree for {key}"),
                }
            }
            if !residue.is_empty() {
                println!("removed {} left by earlier workspaces", residue.len());
            }
            if warm > 0 {
                println!("removed {warm} warm build tree(s) — the next build here is cold");
            }
        }
        return 0;
    }

    if !repo.join("Cargo.toml").is_file() {
        eprintln!(
            "wsp: {} is not a cargo project — verify only knows how to build these",
            util::contract(&repo)
        );
        return 2;
    }

    let Some(head) = git(&repo, &["rev-parse", "HEAD"]).map(|s| s.trim().to_string()) else {
        eprintln!("wsp: {} has no HEAD to build against", util::contract(&repo));
        return 2;
    };

    if let Err(e) = std::fs::create_dir_all(&dir) {
        eprintln!("wsp: cannot make {}: {e}", util::contract(&dir));
        return 2;
    }

    // Step 1 and 2 of the commit procedure: a private index at HEAD, and the
    // file list read before the hunks.
    let index = dir.join("index");
    let patch_path = dir.join("patch.diff");
    let patch = match build_patch(&repo, &cwd, &index, &dir, &args.rest) {
        Ok(p) => p,
        Err(e) => {
            eprintln!("wsp: {e}");
            return 1;
        }
    };
    let files = staged_files(&index, &repo);
    if let Err(e) = std::fs::write(&patch_path, &patch) {
        eprintln!("wsp: cannot write {}: {e}", util::contract(&patch_path));
        return 2;
    }

    let left_out: Vec<String> =
        changed_files(&repo).into_iter().filter(|f| !files.contains(f)).collect();

    if !json_out {
        println!("{} {}", p.dim("head"), &head[..head.len().min(7)]);
        if files.is_empty() {
            println!("{}", p.dim("no change against HEAD — verifying HEAD itself"));
        } else {
            println!("{} {}", p.dim("under test"), p.bold(&format!("{} file(s)", files.len())));
            for f in &files {
                println!("  {f}");
            }
        }
        if !left_out.is_empty() {
            println!(
                "{} {}",
                p.yellow("not under test"),
                p.dim(&format!("{} file(s) changed and left out", left_out.len()))
            );
            for f in &left_out {
                println!("  {}", p.dim(f));
            }
        }
    }

    // A warm tree if there is one free, and the private cold one if there is
    // not — see [`crate::sharing`] for the 23s against 3s that makes this the
    // whole point of the command, and for why nobody queues for it. Held until
    // the build is done, and only the tree and its target move: the index, the
    // patch and the log stay in this agent's own directory, where two agents
    // cannot overwrite each other's.
    let warm = sharing::warm(&store.state, &named_for, sharing::WARM_TREES);
    let (tree, target) = match &warm {
        Some(w) => (w.tree(), w.target()),
        None => (tree, target),
    };
    // Where this build put its artefacts, for `wsp install` to read: it looks
    // for the release binary this command produced, and after this change that
    // is no longer a path it can work out from the checkout alone.
    let _ = std::fs::write(&dir.join(BUILT_AT), format!("{}\n", target.display()));

    let started = Instant::now();
    let fresh = match ensure_tree(&repo, &tree, &head) {
        Ok(f) => f,
        Err(e) => {
            eprintln!("wsp: {e}");
            return 1;
        }
    };
    if !json_out && fresh {
        println!("{} {}", p.dim("new build tree"), util::contract(&tree));
    }

    if !patch.trim().is_empty() {
        if let Err(e) = git_ok(&tree, &["apply", "--binary", &patch_path.display().to_string()]) {
            eprintln!("wsp: the change does not apply to HEAD: {e}");
            eprintln!("wsp: the patch is at {}", util::contract(&patch_path));
            return 1;
        }
    }

    let mut steps: Vec<(&str, Vec<&str>)> = Vec::new();
    if args.has("check") {
        steps.push(("check", vec!["check", "--quiet"]));
    } else {
        steps.push(("test", vec!["test", "--quiet"]));
    }
    if args.has("release") {
        steps.push(("release", vec!["build", "--release", "--quiet"]));
    }

    // Registered here rather than at the top of the command: the share is
    // held for as long as it is being used, so a verify that spends a minute
    // resolving a patch is not counted against the builds running while it does.
    let share = sharing::take(&store.state);
    if !json_out {
        if let Some(note) = share.note() {
            println!("{} {}", p.dim("machine"), p.dim(&note));
        }
    }

    let mut failed: Option<&str> = None;
    let mut output = String::new();
    for (name, argv) in &steps {
        if !json_out {
            println!("{} cargo {}", p.dim("→"), argv.join(" "));
        }
        let (ok, text) = cargo(&tree, &target, argv, !json_out, &share);
        output.push_str(&text);
        if !ok {
            failed = Some(name);
            break;
        }
    }
    let secs = started.elapsed().as_secs_f64();

    // Everything a red run knows, taken before anything else can happen to it.
    // The log goes beside `patch.diff` in the scratch directory — which the
    // tree it belongs to already owns and already removes — so keeping it costs
    // nothing that was not already being kept, and a path in the error line is
    // worth more than any summary printed in its place.
    let mut log: Option<PathBuf> = None;
    let mut failed_tests: Vec<Failure> = Vec::new();
    let mut alone: Option<bool> = None;
    if failed.is_some() {
        let path = dir.join("cargo.log");
        if std::fs::write(&path, &output).is_ok() {
            log = Some(path);
        }
        failed_tests = failures(&output);
        if let Some(first) = failed_tests.first() {
            alone = rerun_alone(&tree, &target, &first.name, &share);
        }
    }

    if json_out {
        // The tail rather than the whole thing: a failing cargo run is
        // thousands of lines and the last eighty hold the error.
        let tail: Vec<&str> = output.lines().rev().take(80).collect::<Vec<_>>().into_iter().rev().collect();
        println!(
            "{}",
            json!({
                "ok": failed.is_none(),
                "failed": failed,
                "head": head,
                "agent": key,
                "files": files,
                "left_out": left_out,
                "tree": util::contract(&tree),
                "checkout": checkout.as_deref().map(util::contract),
                "patch": util::contract(&patch_path),
                "log": log.as_deref().map(util::contract),
                "failures": failed_tests
                    .iter()
                    .map(|f| json!({"test": f.name, "at": f.at, "message": f.message}))
                    .collect::<Vec<_>>(),
                "alone": alone,
                "jobs": share.jobs,
                "cores": share.cores,
                "builds": share.live,
                "warm": warm.as_ref().map(|w| w.slot),
                "seconds": (secs * 10.0).round() / 10.0,
                "output": tail.join("\n"),
            })
        );
        return i32::from(failed.is_some());
    }

    match failed {
        None => {
            println!("{} {} in {:.0}s", p.green("✓"), p.bold("verified against HEAD"), secs);
            println!("{}", p.dim(&format!("tree kept warm at {}", util::contract(&tree))));
            0
        }
        Some(step) => {
            println!("{} cargo {step} failed in {:.0}s", p.red("✗"), secs);
            for f in &failed_tests {
                match &f.at {
                    Some(at) => println!("  {} {}", p.bold(&f.name), p.dim(at)),
                    None => println!("  {}", p.bold(&f.name)),
                }
                if let Some(m) = &f.message {
                    println!("    {}", util::truncate(m, 200));
                }
            }
            if let Some(first) = failed_tests.first() {
                match alone {
                    // The flake signature, and the reason the re-run happens at
                    // all. Yellow because it is the interesting answer, not
                    // green: the suite is still red and still failed.
                    Some(true) => println!(
                        "{}",
                        p.yellow(&format!("{} failed in the suite, passed alone", first.name))
                    ),
                    Some(false) => println!("{}", p.dim(&format!("{} fails alone too", first.name))),
                    None => {}
                }
            }
            // "left at" is now only true until somebody else claims that tree
            // and resets it to their HEAD, which is the price of it being warm.
            // What survives is the log and the patch: they are written in this
            // agent's own directory precisely so that a red run's evidence does
            // not depend on a tree it does not own.
            match &log {
                Some(l) => println!(
                    "{}",
                    p.dim(&format!(
                        "built in {} — cargo output {}, patch {}",
                        util::contract(&tree),
                        util::contract(l),
                        util::contract(&patch_path)
                    ))
                ),
                None => println!(
                    "{}",
                    p.dim(&format!(
                        "built in {} — the patch is {}",
                        util::contract(&tree),
                        util::contract(&patch_path)
                    ))
                ),
            }
            1
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn scratch_dir(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("wsp-verify-{}-{}", name, std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    /// A repository with one commit, and `git` configured enough to make it.
    fn repo(dir: &Path) {
        let run = |args: &[&str]| {
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
        };
        run(&["init", "--quiet", "-b", "master"]);
        std::fs::write(dir.join("kept.txt"), "one\n").unwrap();
        run(&["add", "kept.txt"]);
        run(&["commit", "--quiet", "-m", "first"]);
    }

    /// The failure this command exists to make impossible, asserted directly.
    ///
    /// `git worktree add` writes the new worktree's index, and with
    /// `GIT_INDEX_FILE` exported it writes it over the private one — so the
    /// patch comes back empty and `git apply` refuses it, which reads like a
    /// staging mistake rather than what it is. Every git call here strips the
    /// variable, so a caller partway through `wsp commit-help` with one
    /// exported still gets its own staging read correctly.
    #[test]
    fn a_caller_holding_a_private_index_still_gets_its_own_diff() {
        let dir = scratch_dir("index");
        repo(&dir);
        std::fs::write(dir.join("kept.txt"), "one\ntwo\n").unwrap();

        // Somebody else's index, exported over us, holding nothing at all.
        let theirs = dir.join("their-index");
        std::env::set_var("GIT_INDEX_FILE", &theirs);

        let scratch = dir.join("scratch");
        std::fs::create_dir_all(&scratch).unwrap();
        let mine = scratch.join("index");
        let patch = build_patch(&dir, &dir, &mine, &scratch, &[]).unwrap();
        std::env::remove_var("GIT_INDEX_FILE");

        assert!(patch.contains("+two"), "the change was not in the patch:\n{patch}");
        assert!(!theirs.exists(), "we wrote to the caller's index");
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// A new file is the commonest thing an agent adds and the easiest thing
    /// for a diff to miss — `git diff HEAD` does not see it, and a build that
    /// silently leaves it out goes green for a change that does not compile.
    #[test]
    fn a_file_git_has_never_seen_is_still_under_test() {
        let dir = scratch_dir("untracked");
        repo(&dir);
        std::fs::write(dir.join("new.rs"), "fn f() {}\n").unwrap();

        // Deliberately inside the repository, which is the case the exclude
        // guard is for: a state directory under the tree would otherwise put
        // verify's own index and patch into the patch.
        let scratch = dir.join("scratch");
        std::fs::create_dir_all(&scratch).unwrap();
        let index = scratch.join("index");
        let patch = build_patch(&dir, &dir, &index, &scratch, &[]).unwrap();
        assert!(patch.contains("new.rs"), "an untracked file was not in the patch:\n{patch}");
        assert_eq!(
            staged_files(&index, &dir),
            vec!["new.rs".to_string()],
            "verify staged its own scratch directory"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// Naming paths is the whole discipline: the tree is shared, so "everything
    /// that changed" is not the same question as "what I changed".
    #[test]
    fn naming_paths_leaves_the_other_agents_work_out_of_it() {
        let dir = scratch_dir("paths");
        repo(&dir);
        std::fs::write(dir.join("mine.txt"), "mine\n").unwrap();
        std::fs::write(dir.join("theirs.txt"), "theirs\n").unwrap();

        let scratch = dir.join("scratch");
        std::fs::create_dir_all(&scratch).unwrap();
        let index = scratch.join("index");
        let patch = build_patch(&dir, &dir, &index, &scratch, &["mine.txt".to_string()]).unwrap();
        assert!(patch.contains("mine.txt"));
        assert!(!patch.contains("theirs.txt"), "another agent's file rode along:\n{patch}");
        assert_eq!(staged_files(&index, &dir), vec!["mine.txt".to_string()]);
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// Naming paths keeps somebody else's work out, and lets you leave out your
    /// own. The second is the dangerous one: a new module nothing declares
    /// compiles perfectly, because nothing compiles it — a green build for a
    /// change that does not build. So what was left out has to be visible
    /// beside what was taken.
    #[test]
    fn what_you_left_out_is_reported_next_to_what_you_named() {
        let dir = scratch_dir("leftout");
        repo(&dir);
        std::fs::write(dir.join("mine.txt"), "mine\n").unwrap();
        std::fs::write(dir.join("forgot.txt"), "also mine\n").unwrap();

        let scratch_dir = dir.join("scratch");
        std::fs::create_dir_all(&scratch_dir).unwrap();
        let index = scratch_dir.join("index");
        build_patch(&dir, &dir, &index, &scratch_dir, &["mine.txt".to_string()]).unwrap();

        let named = staged_files(&index, &dir);
        let left_out: Vec<String> =
            changed_files(&dir).into_iter().filter(|f| !named.contains(f)).collect();
        assert_eq!(named, vec!["mine.txt".to_string()]);
        assert!(
            left_out.contains(&"forgot.txt".to_string()),
            "a changed file left out of the build was not reported: {left_out:?}"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// The tree is keyed on the workspace rather than the pane because pane ids
    /// are reissued across herdr restarts — claims are keyed the same way, for
    /// the same reason. A tree that changed identity on every restart would be
    /// a cold build on every restart.
    #[test]
    fn the_build_tree_is_this_agents_and_not_this_panes() {
        std::env::set_var("WSP_AGENT", "w1");
        assert_eq!(agent_key(), "w1");
        std::env::set_var("WSP_AGENT", "  ");
        std::env::remove_var("HERDR_WORKSPACE_ID");
        assert_eq!(agent_key(), "solo");
        std::env::remove_var("WSP_AGENT");

        let store = Store::at(PathBuf::from("/tmp/store"), PathBuf::from("/tmp/state"));
        let a = build_dir(&store, Path::new("/Users/x/claude/wsp"), "w1");
        let b = build_dir(&store, Path::new("/Users/x/claude/wsp"), "w2");
        assert_ne!(a, b, "two agents shared one build tree");
        assert!(a.starts_with("/tmp/state/build"), "the build tree escaped WSP_STATE: {a:?}");

        // The trunk names it, so an agent that moves task keeps the tree it
        // warmed. What resolves the trunk is tested where it lives, in
        // `cmd_checkout`.
        assert_eq!(
            a,
            build_dir(&store, Path::new("/Users/x/claude/wsp"), "w1"),
            "one agent, two tasks, two build trees"
        );
    }

    /// The saving this command was rebuilt for: an agent standing in its own
    /// checkout builds *inside* it, so the build goes when the tree does and
    /// what was unbounded becomes one per live task.
    #[test]
    fn inside_a_checkout_the_build_goes_in_the_checkout_and_dies_with_it() {
        let store = Store::at(PathBuf::from("/tmp/store"), PathBuf::from("/tmp/state"));
        let checkout = PathBuf::from("/Users/x/claude/wsp/.worktrees/robustness-046");

        let sc = scratch(&store, &checkout, "w1");
        assert_eq!(sc.checkout.as_deref(), Some(checkout.as_path()));
        for p in [&sc.dir, &sc.tree, &sc.target] {
            assert!(p.starts_with(&checkout), "{p:?} outlives the tree it belongs to");
            // Under `target/`, which every cargo project already gitignores. A
            // scratch directory git could see would make the checkout
            // permanently dirty, and `checkout --sweep` refuses to remove a
            // dirty tree — so the leak this closes would come straight back.
            assert!(p.starts_with(checkout.join("target")), "{p:?} is not gitignored");
        }
    }

    /// The measurement that settled the shape, asserted so it cannot be
    /// quietly undone. Pointing both trees at one `CARGO_TARGET_DIR` is the
    /// obvious saving and it is wrong: cargo records a unit's dependencies as
    /// absolute paths and judges freshness by their mtimes, so a build in the
    /// scratch tree leaves the *checkout's* fingerprint pointing at the scratch
    /// tree's sources. The agent's next `cargo test` then asks whether the
    /// scratch tree changed, prints `Fresh`, and reruns the old binary —
    /// observed here on 2026-08-17, a green 482 tests for source that had never
    /// been compiled. A wrong green is what this whole command exists to stop.
    #[test]
    fn the_scratch_never_builds_into_the_tree_the_agent_edits_in() {
        let store = Store::at(PathBuf::from("/tmp/store"), PathBuf::from("/tmp/state"));
        let checkout = PathBuf::from("/Users/x/claude/wsp/.worktrees/robustness-046");
        let sc = scratch(&store, &checkout, "w1");
        assert_ne!(
            sc.target,
            checkout.join("target"),
            "the scratch shares the checkout's target dir — `cargo test` there now lies"
        );
        assert_eq!(sc.target, sc.dir.join("target"), "the scratch built somewhere it does not own");
    }

    /// The trunk is still shared — the coordination seat stands there — so the
    /// original argument still holds there, and only there.
    #[test]
    fn in_the_trunk_an_agent_still_builds_in_a_tree_of_its_own() {
        let store = Store::at(PathBuf::from("/tmp/store"), PathBuf::from("/tmp/state"));
        let sc = scratch(&store, Path::new("/tmp/not-a-checkout"), "w1");
        assert_eq!(sc.checkout, None);
        assert!(
            sc.target.starts_with("/tmp/state/build"),
            "the trunk's build escaped WSP_STATE: {:?}",
            sc.target
        );
        assert_eq!(sc.target, sc.dir.join("target"), "the trunk shares its target dir");
    }

    /// A build no longer lands where the checkout says it does — it lands in
    /// whichever warm tree was free — so the one caller that wants the
    /// *artefacts* rather than a place to build has to be told, and this is the
    /// telling. `wsp install` looking in the wrong place is not a small bug:
    /// it falls through to `target/release` and installs somebody else's build
    /// while reporting it as yours.
    #[test]
    fn install_is_told_which_tree_the_build_actually_went_to() {
        let iso = util::isolated("verify-built-at");
        let store = Store::at(iso.home(), iso.state());
        let repo = iso.path("wsp");
        let sc = scratch(&store, &repo, "w1");
        std::fs::create_dir_all(&sc.dir).unwrap();
        let warm = iso.path("state/warm/wsp-0/target");
        std::fs::create_dir_all(&warm).unwrap();
        std::fs::write(sc.dir.join(BUILT_AT), format!("{}\n", warm.display())).unwrap();

        let found = last_build(&store, &repo, "w1");
        assert_eq!(found.target, warm, "install would have looked in the private tree");
        assert_eq!(found.tree, warm.parent().unwrap().join("tree"), "the tree beside it");
        assert_eq!(found.dir, sc.dir, "the patch and the log are still this agent's own");
    }

    /// And a pointer at a tree that has been removed is not an answer. `--rm`
    /// takes warm trees away, so the pointer outliving one is ordinary rather
    /// than exotic — and "no build" is the right answer then, not a path that
    /// no longer exists.
    #[test]
    fn a_pointer_at_a_tree_that_is_gone_falls_back_to_this_agents_own() {
        let iso = util::isolated("verify-built-gone");
        let store = Store::at(iso.home(), iso.state());
        let repo = iso.path("wsp");
        let sc = scratch(&store, &repo, "w1");
        std::fs::create_dir_all(&sc.dir).unwrap();
        std::fs::write(sc.dir.join(BUILT_AT), "/tmp/warm-tree-that-was-removed/target\n").unwrap();

        assert_eq!(last_build(&store, &repo, "w1").target, sc.target);
    }

    /// The residue of keying on the workspace, and the only command that can
    /// reach it: the workspaces that named those trees are gone, so no agent
    /// alive can name one. 9.6G across 30 of them, measured 2026-08-17.
    ///
    /// A live workspace keeps its tree. The name is `<repo>-<workspace>` and a
    /// repository name may hold a dash, so the match is on the tail — `wsp-w2x`
    /// and `my-wsp-w2x` are both workspace `w2x`, and neither is workspace `2x`.
    #[test]
    fn a_tree_is_cleared_when_its_workspace_is_gone_and_kept_while_it_is_not() {
        let dir = scratch_dir("residue");
        let store = Store::at(dir.join("store"), dir.join("state"));
        let build = store.state.join("build");
        for t in ["wsp-w2x", "wsp-w2y", "my-wsp-w2z"] {
            std::fs::create_dir_all(build.join(t).join("tree")).unwrap();
        }
        let live = vec!["w2y".to_string(), "w2z".to_string()];

        let gone = clear_build_dirs(&store, Some(&live), Path::new("/nowhere"));
        assert_eq!(gone.len(), 1, "the wrong trees went: {gone:?}");
        assert!(!build.join("wsp-w2x").exists(), "a tree nobody can name was left behind");
        assert!(build.join("wsp-w2y").exists(), "a live agent lost the tree it was building in");
        assert!(build.join("my-wsp-w2z").exists(), "the workspace was matched as part of the repo");

        // Nothing to clear is not an error: `--rm --all` is a thing you run
        // without first checking whether there is anything to run it on.
        assert!(clear_build_dirs(&store, Some(&live), Path::new("/nowhere")).is_empty());
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// The one line standing between a herdr that is down and every running
    /// agent losing the tree it is mid-build in. A herdr that cannot be reached
    /// reports no workspaces, which looks exactly like a machine with nothing
    /// running on it — so `None` removes nothing, and an empty list is believed
    /// only because herdr said it. The same judgement `may_reap` makes, and for
    /// the same reason.
    #[test]
    fn a_herdr_that_did_not_answer_is_not_a_machine_with_no_agents_on_it() {
        let dir = scratch_dir("silence");
        let store = Store::at(dir.join("store"), dir.join("state"));
        let build = store.state.join("build");
        std::fs::create_dir_all(build.join("wsp-w2x").join("tree")).unwrap();

        assert!(
            clear_build_dirs(&store, None, Path::new("/nowhere")).is_empty(),
            "silence from herdr was read as nothing running"
        );
        assert!(build.join("wsp-w2x").exists(), "a tree went on herdr saying nothing");

        // Said, and meant: an empty answer from a herdr that answered is a
        // machine with no agents on it, and its trees are residue.
        assert_eq!(clear_build_dirs(&store, Some(&[]), Path::new("/nowhere")).len(), 1);
        let _ = std::fs::remove_dir_all(&dir);
    }
    /// Everything this command was failing to say, said. A run that goes red on
    /// this suite is usually the only run that will — the next one is green —
    /// so the name and the assertion have to come out of the output that
    /// already exists rather than out of a second run that no longer fails.
    ///
    /// The fixture is a real `cargo test --quiet` run, verbatim, because what
    /// is being parsed is somebody else's format and a fixture we wrote to suit
    /// the parser would prove only that the parser matches itself.
    #[test]
    fn a_red_run_names_the_test_and_what_its_assertion_said() {
        let out = "\nrunning 2 tests\n. 1/2\nt::a_thing_that_does_not_hold --- FAILED\n\nfailures:\n\n---- t::a_thing_that_does_not_hold stdout ----\n\nthread 't::a_thing_that_does_not_hold' (104182998) panicked at src/main.rs:7:39:\nassertion `left == right` failed: the counts differ\n  left: 1\n right: 2\nnote: run with `RUST_BACKTRACE=1` environment variable to display a backtrace\n\n\nfailures:\n    t::a_thing_that_does_not_hold\n\ntest result: FAILED. 1 passed; 1 failed; 0 ignored; 0 measured; 0 filtered out; finished in 0.00s\n\nerror: test failed, to rerun pass `--bin q`\n";

        let f = failures(out);
        assert_eq!(f.len(), 1, "the failing test was not found: {f:?}");
        assert_eq!(f[0].name, "t::a_thing_that_does_not_hold");
        assert_eq!(f[0].at.as_deref(), Some("src/main.rs:7:39"));
        assert_eq!(
            f[0].message.as_deref(),
            Some("assertion `left == right` failed: the counts differ"),
            "the assertion was dropped and only the name kept"
        );
    }

    /// The list is the authoritative set and the blocks are not: a harness that
    /// prints one and shortens the other still has to leave a name behind. A
    /// failure reported by name alone is worth having — it is the thing you
    /// cannot get back from a green re-run.
    #[test]
    fn a_failure_with_no_detail_block_is_still_named() {
        let out = "\nfailures:\n    panel::keys::a_key_that_moves\n    store::an_id_is_never_reissued\n\ntest result: FAILED. 480 passed; 2 failed; 0 ignored\n";
        let f = failures(out);
        assert_eq!(
            f.iter().map(|x| x.name.as_str()).collect::<Vec<_>>(),
            vec!["panel::keys::a_key_that_moves", "store::an_id_is_never_reissued"]
        );
        assert!(f[0].message.is_none(), "a message was invented for a test that printed none");
    }

    /// A compile failure names no test, and must not appear to. Cargo has
    /// already printed the error in full; a fabricated test path beside it
    /// would be worse than the silence this replaces.
    #[test]
    fn a_build_that_did_not_compile_invents_no_test_name() {
        let out = "error[E0425]: cannot find value `rows` in this scope\n  --> src/panel/rows.rs:12:9\nerror: could not compile `wsp` (bin \"wsp\") due to 1 previous error\n";
        assert!(failures(out).is_empty(), "a compile error was reported as a failing test");
    }

    /// The re-run alone is a measurement, so it declines to guess. A doc test
    /// is named `src/store.rs - Store::at (line 12)`, which libtest cannot be
    /// handed as an exact filter — handing it over anyway would run the whole
    /// suite again under the name of one test, which is the retry this command
    /// exists not to do.
    #[test]
    fn a_name_libtest_cannot_filter_on_is_not_rerun_at_all() {
        let nowhere = Path::new("/nowhere");
        let share = sharing::unregistered();
        assert_eq!(
            rerun_alone(nowhere, nowhere, "src/store.rs - Store::at (line 12)", &share),
            None
        );
        assert_eq!(rerun_alone(nowhere, nowhere, "", &share), None);
    }
}
