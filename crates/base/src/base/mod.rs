//! The bottom layer's command line, git, and the names every other layer
//! shares. It owns no state. The build stamp stays in the `wsp` package, where
//! `build.rs` writes the variables it reads.

pub mod args;
pub mod git;
pub mod labels;
pub mod procs;
pub mod seat;
pub mod shell;
pub mod term;
