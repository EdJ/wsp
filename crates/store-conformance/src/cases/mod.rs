//! The ten cases of 2.3, each a `pub fn case_x<H: Harness>(h: &H)` that panics
//! when the backend breaks its guarantee, so a backend's tests call them one
//! by one with no exceptions file. Concurrency cases race threads and bound
//! their waits in seconds; none sleeps hoping for an interleaving.
//!
//! Cases 1-4, 9 and 10 come with wsp-047's leg `state`; 5-8 and the bite tests
//! (each [`Knobs`](crate::mem::Knobs) makes its case fail) with leg `events`.
