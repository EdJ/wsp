//! `wsp-testkit`: the git repository fixture every test module copies instead of
//! running `git init`. The store-backed half (`store`, `isolated_store`) stays
//! in `src/testkit.rs` of the `wsp` package, beside `Store`.
//!
//! The template is made once per test process (`git init` on `master`, the test
//! identity in its config, an empty hooks directory), and [`repo`] copies it.
//! [`repo_at`] adds files and makes the first commit; [`git`] runs git in a copy
//! with the identity the commits need.

use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::OnceLock;

/// `git -C dir args…` with the test identity and no inherited index, asserting
/// it succeeded, and its stdout.
pub fn git(dir: &Path, args: &[&str]) -> String {
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
    String::from_utf8_lossy(&out.stdout).into_owned()
}

/// Writes an executable script (mode 0755) at `path` from a child process,
/// never from this one: a test thread that forks while this process holds the
/// file open for writing hands the fd to its child, and on Linux exec of the
/// script then fails with ETXTBSY ("Text file busy") until that child execs.
pub fn script(path: &Path, body: &str) {
    let mut child = Command::new("sh")
        .arg("-c")
        .arg("cat > \"$1\" && chmod 755 \"$1\"")
        .arg("sh")
        .arg(path)
        .stdin(std::process::Stdio::piped())
        .spawn()
        .unwrap();
    std::io::Write::write_all(child.stdin.as_mut().unwrap(), body.as_bytes()).unwrap();
    drop(child.stdin.take());
    assert!(child.wait().unwrap().success(), "writing {}", path.display());
}

/// A stub `gh` first on `PATH` until dropped: `pr list --head <branch>` for
/// the open PR it was made with is answered with that PR, any other question
/// with `[]`. `PATH` is the process's, so the caller holds the environment lock
/// (`util::isolated`) for as long as this lives, and drops this first.
pub struct StubGh {
    path: Option<std::ffi::OsString>,
}

impl StubGh {
    /// The script is written in `bin`, which is created.
    pub fn open_pr(bin: &Path, branch: &str, url: &str, head_oid: &str) -> StubGh {
        std::fs::create_dir_all(bin).unwrap();
        let pr = format!(r#"[{{"url":"{url}","headRefOid":"{head_oid}","headRefName":"{branch}"}}]"#);
        let asked = format!("pr list --head {branch} --state open --json ");
        script(&bin.join("gh"), &format!("#!/bin/sh\ncase \"$*\" in\n'{asked}'*) echo '{pr}' ;;\n*) echo '[]' ;;\nesac\n"));
        let path = std::env::var_os("PATH");
        let rest = path.iter().flat_map(std::env::split_paths);
        std::env::set_var("PATH", std::env::join_paths(std::iter::once(bin.to_path_buf()).chain(rest)).unwrap());
        StubGh { path }
    }
}

impl Drop for StubGh {
    fn drop(&mut self) {
        match &self.path {
            Some(path) => std::env::set_var("PATH", path),
            None => std::env::remove_var("PATH"),
        }
    }
}

/// The empty repository every [`repo`] is a copy of, made on first use.
fn template() -> &'static Path {
    static TEMPLATE: OnceLock<PathBuf> = OnceLock::new();
    TEMPLATE.get_or_init(|| {
        let dir = std::env::temp_dir().join(format!("wsp-testkit-repo-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        git(&dir, &["init", "--quiet", "-b", "master"]);
        for (key, value) in [("user.name", "t"), ("user.email", "t@t"), ("commit.gpgsign", "false")] {
            git(&dir, &["config", key, value]);
        }
        let hooks = dir.join(".git/hooks");
        let _ = std::fs::remove_dir_all(&hooks);
        std::fs::create_dir_all(&hooks).unwrap();
        dir
    })
}

fn copy_tree(from: &Path, to: &Path) {
    std::fs::create_dir_all(to).unwrap();
    for entry in std::fs::read_dir(from).unwrap() {
        let entry = entry.unwrap();
        let target = to.join(entry.file_name());
        if entry.file_type().unwrap().is_dir() {
            copy_tree(&entry.path(), &target);
        } else {
            std::fs::copy(entry.path(), &target).unwrap();
        }
    }
}

/// Make `dir` (created if missing) an empty git repository on `master`, as
/// `git init --quiet -b master` would, with the test identity configured.
pub fn repo(dir: &Path) {
    copy_tree(template(), dir);
}

/// [`repo`], then `files` written and committed as the first commit with
/// `message`; the commit's sha.
pub fn repo_at(dir: &Path, files: &[(&str, &str)], message: &str) -> String {
    repo(dir);
    for (name, text) in files {
        let path = dir.join(name);
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).unwrap();
        }
        std::fs::write(path, text).unwrap();
    }
    git(dir, &["add", "-A"]);
    git(dir, &["commit", "--quiet", "-m", message]);
    git(dir, &["rev-parse", "HEAD"]).trim().to_string()
}

