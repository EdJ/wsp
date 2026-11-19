//! `wsp-store-conformance`: the state store's conformance suite
//! (docs/state-core.md 2.3), generic over a [`Harness`], and [`mem::MemStore`],
//! the in-memory fake it first runs against. A backend is done when it passes
//! every case, with no exceptions file. Depends on `wsp-store-contract` and
//! `wsp-testkit` only (section 6). See `MAP.md`.

pub mod cases;
pub mod domain;
pub mod harness;
pub mod mem;

pub use domain::TestDomain;
pub use harness::{Harness, MemHarness};
pub use mem::{Knobs, MemStore};
