//! What this binary was built from, as the engine's modules ask it.
//!
//! The root's `build.rs` writes the commit and the dirt flag into the root
//! package (`src/base/stamp.rs`, `COMMIT` and `DIRTY`), which this crate cannot
//! read; so the binary hands its `build_stamp` in, as it does `store::BUILD_STAMP`.

use std::sync::OnceLock;

/// Set once by the binary (`main`) to the root's `base::stamp::build_stamp`.
/// Unset, the stamp is empty, which every reader takes as a binary that cannot
/// say where it came from.
pub static BUILD_STAMP: OnceLock<fn() -> String> = OnceLock::new();

/// What this binary was built from, as one word: `c52f3c8`, `c52f3c8+dirty`,
/// or empty when the tree was not a checkout (the root's `build_stamp`).
pub fn build_stamp() -> String {
    BUILD_STAMP.get().map(|f| f()).unwrap_or_else(unset)
}

fn unset() -> String {
    // This crate's own tests ran inside the root package until wsp-440 and
    // read its stamp, never an empty one: a stamp they can tell from none.
    if cfg!(test) {
        return stamp_word("0000000", true);
    }
    String::new()
}

/// The same word, made of a commit and a dirt flag that came from somewhere
/// else — a binary that is not this one, asked what it carries.
///
/// Split out of [`build_stamp`] rather than written twice because the two
/// sides of every comparison in the fleet are made here: a `wsp watch`
/// registers [`build_stamp`], and `wsp install` reads a stamp back out of the
/// artefact it is about to copy and has to produce the same word for the same
/// build. `cmd_install` spelled that rule out a second time and spelled it
/// against the *tree* instead, which is `worklist-042`: two shapes for one
/// question is how they come to disagree.
pub fn stamp_word(commit: &str, dirty: bool) -> String {
    match (commit.is_empty(), dirty) {
        (true, _) => String::new(),
        (false, false) => commit.to_string(),
        (false, true) => format!("{commit}+dirty"),
    }
}
