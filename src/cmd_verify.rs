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
//! # A build tree per *agent*, not per build
//!
//! The standing rule since 2026-08-15 is a detached worktree per build, and it
//! works — seven commits in one day, nothing false caught, nothing true missed.
//! What it costs is a cold build every time: measured on this machine, `cargo
//! check` 14s cold against 1–4s warm, and a release build 1m01s cold. And it
//! leaks, because a cleanup step at the end of a long piece of work is a step
//! that gets abandoned when something more interesting happens — `git worktree
//! list` held four stale ones on the day this was written.
//!
//! So the tree is keyed on the agent and kept. `CARGO_TARGET_DIR` sits beside
//! it and persists, which is where the warmth actually lives; the tree is reset
//! to HEAD and re-patched on each run, so only what you changed rebuilds. One
//! tree per agent to leak rather than one per commit, and `--rm` to drop it.
//!
//! # What this deliberately does not need
//!
//! You keep editing in the shared checkout. The pane's cwd stays under the
//! declared root, so `wsp where`, the panel and `overlap` all go on working,
//! and the `project_for_cwd` prerequisite recorded on t-260815-022 does not
//! apply — that is about the tree you *work* in, and a tree you only build in
//! never has a pane standing in it.

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
fn git(dir: &Path, args: &[&str]) -> Option<String> {
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
fn git_ok(dir: &Path, args: &[&str]) -> Result<(), String> {
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
fn toplevel(dir: &Path) -> Option<PathBuf> {
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

/// Where this agent's build tree lives, under the state directory rather than
/// the store: it is machine-local, it is large, and it is not worth committing.
/// Under `WSP_STATE` rather than a fixed path so a sandbox
/// (see t-260816-056) gets its own and does not warm — or corrupt — the real
/// one.
fn build_dir(store: &Store, repo: &Path, key: &str) -> PathBuf {
    let name = repo.file_name().and_then(|s| s.to_str()).unwrap_or("repo");
    store.state.join("build").join(format!("{}-{}", util::slugify(name), key))
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
/// patch, the build tree. It is normally under `WSP_STATE` and so nowhere near
/// the repository, but nothing guarantees that: a sandbox pointed at a state
/// directory inside the tree would otherwise have `git add -A` stage verify's
/// own index into the patch it is building, which is a change that then fails
/// to apply against HEAD for reasons no one could read.
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
            argv.push(format!(":(exclude){}", rel.display()));
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
    let dir = build_dir(store, &repo, &key);
    let tree = dir.join("tree");
    let target = dir.join("target");

    // `--rm` before anything else: the point of it is a tree you can drop when
    // it has gone wrong, and needing a working repository to drop it would be
    // exactly backwards.
    if args.has("rm") {
        let existed = tree.exists();
        let _ = git(&repo, &["worktree", "remove", "--force", &tree.display().to_string()]);
        let _ = std::fs::remove_dir_all(&dir);
        let _ = git(&repo, &["worktree", "prune"]);
        if json_out {
            println!("{}", json!({"removed": existed, "path": util::contract(&dir)}));
        } else if existed {
            println!("removed {}", util::contract(&dir));
        } else {
            println!("no build tree for {key}");
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

    fn scratch(name: &str) -> PathBuf {
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
        let dir = scratch("index");
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
        let dir = scratch("untracked");
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
        let dir = scratch("paths");
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
        let dir = scratch("leftout");
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
    }
}
