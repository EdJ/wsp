//! The bottom layer, as the root package sees it: the command line, git, labels
//! and the seat names live in `wsp-base` (`crates/base`) and are re-exported at
//! the paths they always had. The build stamp stays here, because `build.rs`
//! writes the variables it reads.

pub(crate) use wsp_base::base::{args, git, labels, procs, seat, term};
pub(crate) mod stamp;
