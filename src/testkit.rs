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
pub(crate) use wsp_testkit::{git, repo, repo_at};

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

