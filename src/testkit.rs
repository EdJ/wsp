//! The test kit as the root package sees it: the store-backed fixtures live
//! beside `Store` in `wsp-store` (`crates/store/src/testkit.rs`) and the git
//! repository fixture in `wsp-testkit`; both are re-exported at the path every
//! test module already uses.

#![cfg(test)]

pub(crate) use wsp_store::testkit::{isolated_store, store};
pub(crate) use wsp_testkit::{git, repo, repo_at};
