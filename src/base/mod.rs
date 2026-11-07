//! The bottom layer: the command line, the build stamp, git, and the names every
//! other layer shares. It owns no state. Each item is re-exported at the path it
//! had before the move, so no caller changes.

pub(crate) mod args;
pub(crate) mod git;
pub(crate) mod labels;
pub(crate) mod seat;
pub(crate) mod stamp;
pub(crate) mod term;
