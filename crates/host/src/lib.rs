//! `wsp-host`: the in-process host (`docs/agent-manager.md` section 6.2, M1;
//! wsp-020); see `MAP.md`.
//!
//! Every OS operation a seat needs, run in this process: spawning and stopping
//! its processes, the process table, signals and working directories, the
//! screen its renderer reports, delivering an order, the hook files and burn
//! tally of its directory, its transcripts, and the host's facts; and the
//! tree-job runner ([`jobs`], M3) that serves `job.run`. It depends
//! on `wsp-base` and `wsp-host-api` and on nothing of `wsp-store` or
//! `wsp-wire` (components.md R1, agent-manager.md 2.3), so the agent manager
//! links it. `wsp-wire`'s `host::Host` trait is the adapter over
//! [`InProcess`], and the wire re-exports what moved down here at its old
//! paths.

pub mod facts;
pub mod hooks;
pub mod inproc;
pub mod jobs;
pub mod port;
pub mod price;
pub mod shed;
pub mod tcp;
#[cfg(any(test, feature = "test-support"))]
pub mod test_sups;

pub use inproc::InProcess;
pub use port::{Delivery, Refusal, Result, Seat, State};
pub use tcp::Tcp;
