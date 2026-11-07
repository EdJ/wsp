//! `wsp-store`: the records (`docs/components.md`, cut `c5`); see `MAP.md`.
//! Depends on `wsp-base` and nothing else in the workspace.

pub(crate) use wsp_base::{base, choices, fm, util};

pub mod caption;
pub mod checks;
pub mod decisions;
pub mod governors;
pub mod agent_kind;
pub mod lapse;
pub mod log;
pub mod message;
pub mod model;
pub mod resolve;
pub mod sharing;
pub mod store;
pub mod tags;
pub mod verbs;
#[cfg(any(test, feature = "test-support"))]
pub mod testkit;
