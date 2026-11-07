//! What this binary was built from.

/// The short commit this binary was built from, or empty when the tree it was
/// built in was not a git checkout. `build.rs` carries the argument for why
/// these exist at all.
pub const COMMIT: &str = env!("WSP_COMMIT");

/// Whether that tree held work no commit does. `+dirty` is the load-bearing
/// half of the stamp: a commit hash describes everything about a build except
/// the patch sitting on top of it, and the patch is what goes missing.
pub const DIRTY: bool = matches!(env!("WSP_DIRTY").as_bytes(), b"1");

/// What this binary was built from, as one word a record can be compared
/// against: `c52f3c8`, `c52f3c8+dirty`, or empty when the tree was not a
/// checkout.
///
/// The same two halves `version()` prints, without the package number, because
/// the readers of this are not people: a file written by one build and read by
/// another asks *were these the same rules*, and `0.1.0` has never moved. The
/// dirt is in it for the reason [`DIRTY`] exists at all — two builds at one
/// commit with different patches on top are two different sets of rules, and
/// the patch is the half that is not written down anywhere else.
///
/// Empty compares equal to empty, so two binaries that cannot say where they
/// came from are treated as one build. That is the honest answer rather than a
/// safe one: there is nothing to compare, and a comparison that always failed
/// would put every reader of a stamp permanently in its unknown branch.
pub fn build_stamp() -> String {
    stamp_word(COMMIT, DIRTY)
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
