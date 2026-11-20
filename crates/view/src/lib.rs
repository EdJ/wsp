//! `wsp-view`: the `--json` projections of `wsp ls`, `show`, `wip`, `worklist
//! ls`, `machine ls`, `throttle` and `spend` as pure functions (wsp-305), so
//! the CLI and web-chrome draw the same shapes from the same code.
//!
//! **Inputs are the contract's entities** (the row's decision d1, 2026-11-20):
//! a [`wsp_store_contract::State`] or its `Row`s, `List`s, `Seat`s and
//! `Machine`s, plus plain extra inputs (stage history, worked, live readings,
//! a clock) that the caller reads. The CLI turns its file reads into those
//! entities with the mirror's own projections (`src/mirror.rs`) before calling
//! here; web-chrome passes its fold straight in. Nothing here reads a file, a
//! socket, the environment or the clock: `tests/layering.rs` holds it to
//! `serde_json` and the contract.
//!
//! [`common`] is what every projection shares: the task's record over a `Row`,
//! the field keys the mirror writes, and the pure helpers copied from
//! `wsp-base`. Each other module is one command's projection.

pub mod common;
/// `ls --json` and `show --json`.
pub mod task;
/// `wip --json`.
pub mod wip;
/// `worklist ls --json`.
pub mod worklist;
/// `machine ls --json`.
pub mod machine;
/// `throttle --json`.
pub mod throttle;
/// `spend --json`.
pub mod spend;
