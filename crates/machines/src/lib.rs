//! `wsp-machines`: the machines of docs/state-core.md section 3, each a pure
//! [`Domain`](wsp_store_contract::Domain) (`decide`, `evolve`) over the
//! contract's types. Section 6's crate: it depends on `wsp-store-contract`
//! alone and does no IO, reads no clock and keeps no state of its own; a
//! backend's `apply` runs it. See `MAP.md`.
//!
//! Step 2 brings the orders machine ([`orders::Orders`], 3.1). The seats, rows
//! and lists machines come with steps 3 and 4. [`identity::Identity`] (wsp-159) decides the hub's users and
//! tokens; [`token`] makes and hashes them.

pub mod identity;
pub mod orders;
pub mod token;

pub use identity::Identity;
pub use orders::{Orders, ATTEMPTS};
