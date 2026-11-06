//! The test kit (`wsp-401`, row `c0-fixtures` of `docs/components.md`): one
//! scratch store and one git fixture for every test module, where each module
//! used to write its own `scratch()` and run `git init` for every repository
//! it stood up. Test-only; at cut `c1` it becomes the `wsp-testkit` dev-crate.
//!
//! **Stores.** [`store`] is a store in a directory of its own under the temp
//! dir, with no environment touched, for tests that pass the store around;
//! [`isolated_store`] is a store at [`util::isolated`]'s `WSP_HOME`, for tests
//! whose code under test calls `Store::open()` or reads the environment.
//!
//! **Repositories.** A test that needs a git repository copies one: the
//! template is made once per test process (`git init` on `master`, the test
//! identity in its config, an empty hooks directory), and [`repo`] copies it, which is
//! file copies instead of a `git` process. [`repo_at`] adds files and makes the
//! first commit; [`git`] runs git in a copy with the identity the commits need.

#![cfg(test)]

use crate::store::Store;
use crate::util;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::OnceLock;

/// A store at `<temp>/wsp-<tag>-<pid>`, emptied first. The tag names the
/// directory, so a module prefixes its own (`msg-…`) to keep clear of another
/// module's tests running beside it.
pub(crate) fn store(tag: &str) -> Store {
    let root = std::env::temp_dir().join(format!("wsp-{tag}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    let store = Store::at(root.clone(), root.join("state"));
    store.ensure_dirs().unwrap();
    store
}

/// A store at the isolated environment's home and state, holding the
/// environment lock for as long as the [`util::Isolated`] lives.
pub(crate) fn isolated_store(tag: &str) -> (util::Isolated, Store) {
    let env = util::isolated(tag);
    let store = Store::at(env.home(), env.state());
    store.ensure_dirs().unwrap();
    (env, store)
}

/// `git -C dir args…` with the test identity and no inherited index, asserting
/// it succeeded, and its stdout.
pub(crate) fn git(dir: &Path, args: &[&str]) -> String {
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
pub(crate) fn repo(dir: &Path) {
    copy_tree(template(), dir);
}

/// [`repo`], then `files` written and committed as the first commit with
/// `message`; the commit's sha.
pub(crate) fn repo_at(dir: &Path, files: &[(&str, &str)], message: &str) -> String {
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

