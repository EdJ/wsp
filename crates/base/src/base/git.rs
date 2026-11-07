//! Git, asked from a directory, and the trunk and worktree names the rest hang from.

use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

use crate::util;

/// Run git somewhere and take its stdout, or `None` if it failed.
///
/// `GIT_INDEX_FILE` is stripped on the way in, always. Two different callers
/// would otherwise be wrong in two different ways: an agent partway through
/// `wsp commit-help` has one exported and every read here would inspect its
/// staging rather than the repository, and `worktree add` would write the new
/// worktree's index over it. The two commands that genuinely want a private
/// index set it themselves, on themselves.
pub fn git(dir: &Path, args: &[&str]) -> Option<String> {
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
pub fn git_ok(dir: &Path, args: &[&str]) -> Result<(), String> {
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
pub fn toplevel(dir: &Path) -> Option<PathBuf> {
    let out = git(dir, &["rev-parse", "--show-toplevel"])?;
    let line = out.trim();
    (!line.is_empty()).then(|| PathBuf::from(line))
}

/// The directory under a repository root that holds the per-task worktrees.
///
/// Dotted so it is out of the way of an `ls`, and one directory rather than one
/// per task at the root so a single `.gitignore` line covers every tree that
/// will ever exist here.
pub const WORKTREES: &str = ".worktrees";

/// The repository's main working tree — the trunk that gets reviewed.
///
/// From `git worktree list`, whose first entry is the main one, rather than by
/// walking up from `.git`: inside a linked worktree `.git` is a file pointing
/// somewhere under the main repository's `.git/worktrees`, and reconstructing
/// the main tree's path from that is arithmetic on git's private layout.
pub fn trunk(dir: &Path) -> Option<PathBuf> {
    let out = git(dir, &["worktree", "list", "--porcelain"])?;
    let line = out.lines().find_map(|l| l.strip_prefix("worktree "))?;
    Some(PathBuf::from(line.trim()))
}

/// The commit the trunk branch of the repository at `root` points at, in full.
pub fn trunk_tip(root: &str) -> Option<String> {
    let repo = toplevel(Path::new(&util::expand(root)))?;
    let trunk = trunk(&repo)?;
    let branch = trunk_branch(&trunk)?;
    git(&trunk, &["rev-parse", &branch]).map(|s| s.trim().to_string()).filter(|s| !s.is_empty())
}

/// The branch the trunk has checked out, or `None` if it is detached.
///
/// What `land` fast-forwards, and what a new task branch starts from. Read from
/// the trunk rather than assumed to be `master`, because the name of the trunk
/// branch is a fact about the repository and not about wsp.
pub fn trunk_branch(trunk: &Path) -> Option<String> {
    let out = git(trunk, &["symbolic-ref", "--quiet", "--short", "HEAD"])?;
    let name = out.trim().to_string();
    (!name.is_empty()).then_some(name)
}
