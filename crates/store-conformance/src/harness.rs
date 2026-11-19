//! What a backend implements so the suite can drive it (2.3): open a store,
//! open more handles on it (cases 2, 3 and 7 race several, as separate
//! processes would), and crash one mid-`apply` and reopen (case 8).
//!
//! A handle names its store: [`Harness::open`] and [`Harness::reopen`] take a
//! handle and open another on the same store, so a backend needs no separate
//! "database" type (MemStore shares an `Arc`; SQLite will keep its path).

use std::sync::Arc;

use wsp_store_contract::{Domain, Store};

use crate::domain::TestDomain;
use crate::mem::{Knobs, MemStore};

/// A backend, as the suite sees it. Each case is a `pub fn case_x<H: Harness>(h: &H)`.
pub trait Harness {
    /// One handle on a store: what a process holds.
    type Handle: Store + Send + Sync + 'static;

    /// A new, empty store whose `apply` validates through `domain`, and its first handle.
    fn create(&self, domain: Arc<dyn Domain + Send + Sync>) -> Self::Handle;

    /// Another handle on `on`'s store.
    fn open(&self, on: &Self::Handle) -> Self::Handle;

    /// Arms the crash hook on `h`: its next `apply` stops midway, after it has
    /// decided and before its write is whole, as if its process were killed.
    /// That `apply` returns `Error::Backend`, and `h` is dead after it.
    fn crash_next_apply(&self, h: &Self::Handle);

    /// Reopens `h`'s store after a crash, from what it durably holds; `h` may be dead.
    fn reopen(&self, h: &Self::Handle) -> Self::Handle;

    /// A new store with the test domain, and `n` handles on it (at least one).
    fn handles(&self, n: usize) -> Vec<Self::Handle> {
        let first = self.create(Arc::new(TestDomain));
        let mut out: Vec<_> = (1..n.max(1)).map(|_| self.open(&first)).collect();
        out.insert(0, first);
        out
    }

    /// A new store with the test domain, and one handle.
    fn fresh(&self) -> Self::Handle {
        self.create(Arc::new(TestDomain))
    }
}

/// The harness for [`MemStore`], with the knobs every store it creates starts with.
#[derive(Clone, Copy, Debug, Default)]
pub struct MemHarness {
    pub knobs: Knobs,
}

impl MemHarness {
    /// A harness whose stores break one guarantee (or more): for the bite tests.
    pub fn with(knobs: Knobs) -> Self {
        MemHarness { knobs }
    }
}

impl Harness for MemHarness {
    type Handle = MemStore;

    fn create(&self, domain: Arc<dyn Domain + Send + Sync>) -> MemStore {
        let s = MemStore::with_domain(domain);
        s.set_knobs(self.knobs);
        s
    }

    fn open(&self, on: &MemStore) -> MemStore {
        on.handle()
    }

    fn crash_next_apply(&self, h: &MemStore) {
        h.crash_next_apply();
    }

    fn reopen(&self, h: &MemStore) -> MemStore {
        h.reopen()
    }
}
