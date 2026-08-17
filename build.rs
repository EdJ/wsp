//! The commit this binary was built from, stamped into it at build time.
//!
//! `wsp --version` answered `0.1.0` from the first commit until this file
//! existed, which is the same as answering nothing: every wsp on the machine
//! says it, so an install built from a dirty shared tree and an install built
//! at HEAD are indistinguishable until you type a verb and find out. On
//! 2026-08-16 that cost five hours — two finished features sat uncommitted,
//! one install carried them and two did not, and the symptom was `K` doing
//! nothing and `wsp flag` answering `unknown command`.
//!
//! So the build writes down what it can see, and the binary carries it: the
//! short commit, and whether the tree it came out of had work in it that no
//! commit holds. `wsp doctor` compares that against the repository's HEAD;
//! `src/cmd_install.rs` holds the weaker question this does not replace: what
//! can be said about a binary still sitting beside the tree it came out of.
//!
//! Three things worth knowing about how it is taken:
//!
//! - **`GIT_INDEX_FILE` is stripped**, as it is everywhere else in this tree
//!   that shells out to git. The commit procedure has every agent staging
//!   through a private index; a `status` run through one of those describes
//!   that agent's staging rather than the tree cargo just compiled.
//!
//! - **Untracked files count as dirt**, matching `cmd_install::provenance`.
//!   A new `src/*.rs` that git has never seen is compiled into the binary like
//!   any other, and a stamp that called that tree clean would be lying in
//!   exactly the direction this whole thing exists to stop.
//!
//! - **A source tree that is not a checkout gets no stamp at all** rather than
//!   a guess. `wsp --version` then prints what it always printed. Every git
//!   call here is allowed to fail — an unpacked tarball, a machine with no
//!   git, a `cargo install` from a registry — and none of them is an error:
//!   the binary still works, it just cannot say where it came from.

use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

fn main() {
    let dir = PathBuf::from(std::env::var("CARGO_MANIFEST_DIR").unwrap_or_else(|_| ".".into()));

    let commit = git(&dir, &["rev-parse", "--short", "HEAD"]).unwrap_or_default();
    // Only ask about dirt once there is a commit to be dirty against: a repo
    // with no commit in it would otherwise report every file as uncommitted
    // work, which is true and useless.
    let dirty = !commit.is_empty()
        && git(&dir, &["status", "--porcelain", "--untracked-files=all"])
            .map(|s| !s.is_empty())
            .unwrap_or(false);

    println!("cargo:rustc-env=WSP_COMMIT={commit}");
    println!("cargo:rustc-env=WSP_DIRTY={}", u8::from(dirty));

    watch(&dir);
}

/// One git call, answering `None` for anything that is not a clean success —
/// no repository, no git, no HEAD.
fn git(dir: &Path, args: &[&str]) -> Option<String> {
    let out = Command::new("git")
        .arg("-C")
        .arg(dir)
        .args(args)
        .env_remove("GIT_INDEX_FILE")
        .stderr(Stdio::null())
        .output()
        .ok()?;
    out.status.success().then(|| String::from_utf8_lossy(&out.stdout).trim().to_string())
}

/// What has to change before this is asked again.
///
/// Naming anything at all turns off cargo's default, which is to re-run a
/// build script when any file in the package changes — so the sources have to
/// be named back, or an edit would compile into a binary still claiming the
/// tree was clean. The two events that move the stamp are exactly these: a
/// source edit, and a commit. Commits do not touch a working file, and they do
/// not touch `HEAD` either when the branch is the thing that moved, so the
/// reflog is what is watched — it is appended to by every commit, checkout,
/// reset and rebase, which is the full set of ways HEAD gets somewhere else.
///
/// Only paths that exist are named. Cargo treats a missing one as changed on
/// every build, and a build script that always re-runs is a crate that always
/// looks stale — the version stamp is not worth a rebuild per `cargo check`.
fn watch(dir: &Path) {
    let mut paths: Vec<PathBuf> = vec![dir.join("src"), dir.join("Cargo.toml"), dir.join("build.rs")];
    // `--git-path` rather than `.git/…`: in a worktree — which is what `wsp
    // verify` and `wsp checkout` both build in — `.git` is a file, and the
    // reflog that moves is the worktree's own.
    for p in ["HEAD", "logs/HEAD"] {
        if let Some(found) = git(dir, &["rev-parse", "--git-path", p]) {
            let found = PathBuf::from(&found);
            paths.push(if found.is_absolute() { found } else { dir.join(found) });
        }
    }
    for p in paths.iter().filter(|p| p.exists()) {
        println!("cargo:rerun-if-changed={}", p.display());
    }
}
