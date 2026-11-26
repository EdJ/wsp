# wsp-spoke

The shared-work add-on: the spoke that logs in to a shared hub and syncs shared work (`docs/components.md`, the `spoke`
row; `docs/multiplayer.md`). Depends on `wsp-engine` and the crates below it (`wsp-base`, `wsp-store`,
`wsp-store-contract`, `wsp-objects`) and the hub's wire, `wsp-hub-protocol`, beside `rustls`, `rustls-native-certs` and
`sha2` for its own hub client; not `wsp-api`, `wsp-client` or `wsp-machines`. `wsp-engine` never depends on it
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
- `hub_client` — the spoke's own client of the hub, speaking `wsp-hub-protocol` only: the TLS/TCP dial with a pinned
  CA, hello, requests and identity commands, subscribe, the sync helper and the id, lease and message verbs, the
  login file and `WSP_HUB_*` (`hubs.json`), certificate expiry, the GitHub login, and token mint/hash
- `cmd_login`, `cmd_hub`, `cmd_publish`, `cmd_allocate` — the verbs of the shared side (`wsp login`, with a token or
  `--github`, `logout`, `hub`, `publish`, `allocate`)

## Does not hold

`addon` (the interface), `door`, `mirror`, `orders`, `address`, `record_workspace`: core. They ask the add-on and
never name it.

## Tests

`cargo test -p wsp-spoke` (75). Its tests log in to a real hub (`wsp-hub`, the old in-tree hub, a dev-dependency, with
`wsp-api` beside it to hand it objects in its own types) over TLS. The GitHub login is tested against a scripted
connection only: the in-tree hub serves no login. They use the
engine's fixtures behind its `test-support` feature (`verification::record`, `cmd_message::tests_govern`);
the orders fixtures (`testkit::orders_db`, `orders_seat`) are its own, because the engine's normal dependencies never reach the sqlite crate. The three drive-lease scenarios are split at the add-on interface: through `cycle::step`, with a
scripted `SharedWork`, in the engine's `cycle` tests; against a real hub, asking the real `Spoke`, in `spoke.rs`. None
is left under `#[cfg(any())]`.
