//! The bottom layer at the path the root gives it: `wsp-base`'s command line,
//! git, labels and seat names, re-exported, and the build stamp, which the
//! binary hands in because the root's `build.rs` writes the variables it reads.

pub use wsp_base::base::{args, git, glyph, labels, procs, seat, term};
pub mod stamp;
