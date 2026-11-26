//! `wsp-spoke`: the shared-work add-on, the hub's spoke side (`docs/components.md`). It logs in to a shared hub and
//! syncs shared work, and plugs into core wsp through `wsp_engine::addon::SharedWork` (`spoke::Spoke`). Core
//! never names this crate; the root installs it (`addon::install`) and registers its verbs (`cmd_login`,
//! `cmd_hub`, `cmd_publish`, `cmd_allocate`).

pub mod cmd_allocate;
pub mod cmd_hub;
pub mod cmd_login;
pub mod cmd_publish;
// After a `stale` (wsp-176): the merge, and the push and stale glue; called by the sync worker (wsp-464, wsp-469).
#[allow(dead_code)]
pub mod hub_cache;
pub mod hub_client;
pub mod hub_ids;
pub mod hub_journal;
pub mod hub_lease;
pub mod hub_switch;
pub mod hub_sync;
#[allow(dead_code)]
pub mod reconcile;
pub mod spoke;
/// A real hub for tests, run as an external program (`WSP_HUB_BIN`, `WSP_HUB_IMAGE`).
#[cfg(any(test, feature = "test-support"))]
pub mod testhub;

/// The test kit at the path every test module uses (the engine's own, for these tests).
#[cfg(test)]
pub(crate) mod testkit {
    pub use wsp_store::testkit::{isolated_store, store};
    pub use wsp_testkit::repo_at;

    use std::sync::atomic::{AtomicU64, Ordering};
    use std::sync::Arc;
    use wsp_store_contract::{Command, Entity, IdempotencyKey, MachineName, Principal, Seat, SeatId, SeatState};

    /// A fresh `state.db` under `dir`, its domain the composed one under `mode` (the engine's `orders` fixture).
    pub fn orders_db(dir: &std::path::Path, mode: wsp_server::Mode) -> wsp_server::SharedStore {
        std::fs::create_dir_all(dir).unwrap();
        let _ = std::fs::remove_file(dir.join("state.db"));
        Arc::new(wsp_store_sqlite::SqliteStore::open(dir.join("state.db"), Arc::new(wsp_server::Composed::new(mode))).unwrap())
    }

    /// `seat` mirrored into `db` in `state`, as the fold puts it (the engine's `orders` fixture).
    pub fn orders_seat(db: &wsp_server::SharedStore, seat: &str, state: SeatState) {
        static N: AtomicU64 = AtomicU64::new(0);
        let s = Seat {
            id: SeatId(seat.into()),
            machine: MachineName("mac".into()),
            kind: "claude".into(),
            model: None,
            state,
            before_lost: None,
            row: None,
            attempt: 0,
            pid: None,
            fields: Default::default(),
        };
        let key = IdempotencyKey(format!("test:seat:{seat}:{}", N.fetch_add(1, Ordering::Relaxed)));
        db.apply(Command::Mirror { put: vec![Entity::Seat(s)], gone: vec![] }, key, Principal::Manager(MachineName("mac".into()))).unwrap();
    }
}
