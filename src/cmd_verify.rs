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

/// One cargo run. Inherits stdio unless the caller wants JSON, because the
/// compiler's own output is the thing being asked for — a summary of a build
/// failure is worth nothing next to the error.
fn cargo(tree: &Path, target: &Path, argv: &[&str], capture: bool) -> (bool, String) {
    let mut cmd = Command::new("cargo");
    cmd.args(argv)
        .current_dir(tree)
        .env("CARGO_TARGET_DIR", target)
        .env_remove("GIT_INDEX_FILE");
    if capture {
        match cmd.output() {
            Ok(out) => {
                let mut text = String::from_utf8_lossy(&out.stdout).into_owned();
                text.push_str(&String::from_utf8_lossy(&out.stderr));
                (out.status.success(), text)
            }
            Err(e) => (false, format!("cargo: {e}")),
        }
    } else {
        match cmd.status() {
            Ok(st) => (st.success(), String::new()),
            Err(e) => {
                eprintln!("wsp: cargo: {e}");
                (false, String::new())
            }
        }
    }
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

    // `--rm` before anything else: the point of it is a tree you can drop when
    // it has gone wrong, and needing a working repository to drop it would be
    // exactly backwards.
    if args.has("rm") {
        let existed = tree.exists();
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
                json!({"removed": existed, "path": util::contract(&dir), "also": residue.len()})
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

    let mut failed: Option<&str> = None;
    let mut output = String::new();
    for (name, argv) in &steps {
        if !json_out {
            println!("{} cargo {}", p.dim("→"), argv.join(" "));
        }
        let (ok, text) = cargo(&tree, &target, argv, json_out);
        output.push_str(&text);
        if !ok {
            failed = Some(name);
            break;
        }
    }
    let secs = started.elapsed().as_secs_f64();

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
            println!("{}", p.dim(&format!("tree left at {} — the patch is {}", util::contract(&tree), util::contract(&patch_path))));
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
}
