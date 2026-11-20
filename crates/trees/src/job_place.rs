//! Where a tree job runs (M3 under T2, wsp-053; named apart from `place`, the
//! seat backends): [`place_of`], the one
//! function a check, a verifier and a barrier take a job's place from. A job
//! is given only its [`JobAt`], never a path on wsp's machine.
//!
//! It lives here, beside `check_tree`, which makes the trees, so `wsp check`
//! (the root crate, re-exported in `check.rs`) and `wsp verify`
//! (`cmd_verify.rs`, wsp-054) both reach it: this crate cannot depend on
//! the root crate. What runs a step at the place (`check::Runner`) stays in
//! the root crate.

use std::path::{Path, PathBuf};

use crate::host::{HostDir, HostId, JobAt};
use crate::util;

/// Whose tree a job runs in: a row's, or a barrier's (`<list>#<group>`, as a
/// barrier's job key names it).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Whose<'a> {
    Row(&'a str),
    Barrier(&'a str),
}

impl Whose<'_> {
    /// The row (or barrier) id a job here is keyed under.
    pub fn id(&self) -> &str {
        match self {
            Whose::Row(r) | Whose::Barrier(r) => r,
        }
    }
}

/// Which of its trees: the row's own (`work`), the tree its check runs in, the
/// verifier's, or the barrier's.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TreeKind {
    Work,
    Check,
    Verify,
    Barrier,
}

impl TreeKind {
    /// `work`, `check`, `verify` or `barrier`: how a job key and a message name it.
    pub fn word(self) -> &'static str {
        match self {
            TreeKind::Work => "work",
            TreeKind::Check => "check",
            TreeKind::Verify => "verify",
            TreeKind::Barrier => "barrier",
        }
    }
}

/// Where a job runs, as [`place_of`] answers it: the host-api's [`JobAt`],
/// which is all a job is given, and the tree as this process reaches it for
/// what is still done here (the checkout, the lock, "tree missing").
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Place {
    pub at: JobAt,
    /// The tree on this machine. At M4, when the tree is made on its host by
    /// `tree.checkout`, this goes, and nothing a job is sent reads it.
    pub dir: PathBuf,
    /// The row (or barrier) a job here is keyed under.
    pub row: String,
    pub kind: TreeKind,
}

impl Place {
    /// The host the place is on.
    pub fn host(&self) -> &str {
        match &self.at {
            JobAt::Tree(t) => &t.host.0,
            JobAt::Dir(d) => &d.host.0,
        }
    }
}

/// The host a row's work happens on: the host its seat was spawned on, which
/// holds its trees under T2 (`docs/agent-manager.md` 6.2, "M3 under T2").
/// Until rows record their seat's host (M4) every seat is this machine's.
///
/// A barrier's is the list's barrier host: the trunk's host by default (the
/// barrier stays where the trunk is), which is this machine until the trunk
/// is a remote (M4). A list names no other yet.
pub fn host_of(whose: Whose<'_>) -> String {
    match whose {
        Whose::Row(_) => util::hostname(),
        Whose::Barrier(_) => trunk_host(),
    }
}

/// The trunk's host, where a barrier runs unless its list names another: this
/// machine, which holds the trunk until M4 makes it a remote.
pub fn trunk_host() -> String {
    util::hostname()
}

/// The directory a host's manager keeps the trees it runs jobs in under (its
/// `--tree-root`): the home directory, the manager's default.
pub fn tree_root() -> PathBuf {
    #[cfg(any(test, feature = "test-support"))]
    if let Some(root) = TREE_ROOT.with(|r| r.borrow().clone()) {
        return root;
    }
    util::home()
}

#[cfg(any(test, feature = "test-support"))]
thread_local! {
    static TREE_ROOT: std::cell::RefCell<Option<PathBuf>> = const { std::cell::RefCell::new(None) };
}

/// A test's own tree root, kept to its thread (`None` puts the home back).
#[cfg(any(test, feature = "test-support"))]
pub fn set_tree_root(root: Option<PathBuf>) {
    TREE_ROOT.with(|r| *r.borrow_mut() = root);
}

/// Where `whose`'s `kind` tree is, as every job sent to it is placed: the one
/// function the check, the verifier and the barrier take a job's place from,
/// so none of them joins a path on wsp's machine into a job.
///
/// It makes the tree if it is not there. Until M4 the tree is made here, on
/// this machine, at `tree` as today (its checkout follows under the lock),
/// and answered as a [`HostDir`] on the row's host relative to the tree root.
/// At M4 the body becomes `tree.checkout` on the place's host, `tree` goes,
/// and no caller moves again.
pub fn place_of(whose: Whose<'_>, kind: TreeKind, tree: &Path) -> Result<Place, String> {
    if !tree.is_dir() {
        std::fs::create_dir_all(tree).map_err(|e| format!("could not make the {} tree {}: {e}", kind.word(), util::contract(tree)))?;
    }
    let at = dir_on(&host_of(whose), tree, &format!("the {} tree", kind.word()))?;
    Ok(Place { at: JobAt::Dir(at), dir: tree.to_path_buf(), row: whose.id().to_string(), kind })
}

/// The directory `dir` (on this machine, there already) as a [`HostDir`] on
/// `host`, relative to the tree root: how a job that belongs to no tree (warm's
/// cleanup) is placed. `what` names it in an error.
pub fn dir_on(host: &str, dir: &Path, what: &str) -> Result<HostDir, String> {
    let root = tree_root();
    let (Ok(root_real), Ok(real)) = (root.canonicalize(), dir.canonicalize()) else {
        return Err(format!("{what} {} or the tree root {} does not read", util::contract(dir), util::contract(&root)));
    };
    let Ok(rel) = real.strip_prefix(&root_real) else {
        return Err(format!("{what} {} is not under the tree root {}", util::contract(dir), util::contract(&root)));
    };
    let path = rel.components().map(|c| c.as_os_str().to_string_lossy().into_owned()).collect::<Vec<_>>().join("/");
    let at = HostDir { host: HostId(host.to_string()), path };
    if at.escapes() {
        return Err(format!("{what} {} is the tree root itself or outside it", util::contract(dir)));
    }
    Ok(at)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Every kind is placed by the same function: the row's (or barrier's)
    /// host and a path relative to the tree root, never a path on wsp's
    /// machine; a tree outside the root is no place, and a missing one is made.
    #[test]
    fn every_kind_is_placed_on_its_host_relative_to_the_tree_root() {
        let root = std::env::temp_dir().join(format!("wsp-place-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(&root).unwrap();
        set_tree_root(Some(root.clone()));
        for (whose, kind, sub) in [
            (Whose::Row("pj-001"), TreeKind::Check, "checks/pj-001"),
            (Whose::Row("pj-001"), TreeKind::Verify, "verify/pj-001"),
            (Whose::Row("pj-001"), TreeKind::Work, "trees/pj-001"),
            (Whose::Barrier("m3#5"), TreeKind::Barrier, "barrier/m3/tree"),
        ] {
            let tree = root.join(sub);
            let place = place_of(whose, kind, &tree).unwrap();
            assert!(tree.is_dir(), "{sub} was made");
            let JobAt::Dir(at) = &place.at else { panic!("{:?}", place.at) };
            assert_eq!((at.host.0.as_str(), at.path.as_str()), (host_of(whose).as_str(), sub));
            assert_eq!((place.row.as_str(), place.kind, place.host()), (whose.id(), kind, at.host.0.as_str()));
        }
        assert!(place_of(Whose::Row("pj-001"), TreeKind::Check, Path::new("/")).is_err(), "outside the root");
        assert!(place_of(Whose::Row("pj-001"), TreeKind::Check, &root).is_err(), "the root itself");
        set_tree_root(None);
        let _ = std::fs::remove_dir_all(&root);
    }
}
