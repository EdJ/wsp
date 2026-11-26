# wsp-spoke

The shared-work add-on: the spoke that logs in to a shared hub and syncs shared work (`docs/components.md`, the `spoke`
row; `docs/multiplayer.md`). Depends on `wsp-engine` and the crates below it (`wsp-base`, `wsp-store`,
`wsp-store-contract`, `wsp-objects`, `wsp-api`, `wsp-client`, `wsp-machines`); `wsp-engine` never depends on it
(`tests/layering.rs`). The root installs it and registers its verbs; it re-exports each module at its old path
(`crate::hub_lease`, `crate::cmd_login`, …) in `main.rs`.

## Holds

- `spoke` — `Spoke`, the implementation of `wsp_engine::addon::SharedWork`: every question core asks of the shared
  side, answered from the modules below, and the background sync worker the daemon starts through it
- `hub_switch` — the on/off switch (`wsp hub disable|enable|status`) and the logout that disables first
- `hub_sync` — the sync worker: push, pull, the door, messages to and from other machines
- `hub_lease` — row and drive leases, their renewal, verdicts and landings sent between machines
- `hub_journal`, `hub_cache` — what the spoke keeps of the hub between runs (the offline journal and side chains; the
  cache and the merge after a stale)
- `reconcile` — the merging reconcile the door calls
- `cmd_login`, `cmd_hub`, `cmd_publish`, `cmd_allocate` — the verbs of the shared side (`wsp login`, `logout`, `hub`,
  `publish`, `allocate`)

## Does not hold

`addon` (the interface), `door`, `mirror`, `orders`, `address`, `record_workspace`: core. They ask the add-on and
never name it.

## Tests

`cargo test -p wsp-spoke` (52). Its tests log in to a real hub (`wsp-hub`, a dev-dependency) over TLS. They use the
engine's fixtures behind its `test-support` feature (`verification::record`, `cmd_message::tests_govern`);
the orders fixtures (`testkit::orders_db`, `orders_seat`) are its own, because the engine's normal dependencies never reach the sqlite crate. Three tests that drive `cycle::step` through a shared list (the drive lease) are kept in
`spoke.rs` under `#[cfg(any())]`: the run's recorded spawns and tells exist only in the engine's own test build.
