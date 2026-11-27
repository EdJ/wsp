# wsp-spoke

The shared-work add-on: the spoke that logs in to a shared hub and syncs shared work (`docs/components.md`, the `spoke`
row; `docs/multiplayer.md`). Depends on `wsp-engine` and the crates below it (`wsp-base`, `wsp-store`,
`wsp-store-contract`, `wsp-objects`) and the hub's wire, `wsp-hub-protocol`, beside `rustls`, `rustls-native-certs` and
`sha2` for its own hub client; not `wsp-api`, `wsp-client`, `wsp-machines` or `wsp-seats` (it reaches the seats only
through the engine's installed `cycle::seats()`). Neither `wsp-engine` nor `wsp-seats` depends on it
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
- `hub_ids`, `hub_members` — a shared row's id from the hub (or a provisional one), and the user's workspaces as the
  hub last said, so a filing into one the user is not a member of is refused when it is filed
- `hub_lists` — a shared list whose slug another list here holds (another workspace's, or a private one), kept
  under a local name of its own and renamed to its hub id at the sync's edge
- `hub_holders` — who holds each shared row's lease (user, machine, expiry), as the stream's lease events last said
  (`holders.json`): the rows' `held_by`/`held_on`/`held_until` facts
- `hub_retire` — a shared row moved to a new id: its old subject retired on the hub (`refs.retire`), and the hub's
  `retired` list followed on every spoke (stubs gone, old id resolving to the new one)
- `reconcile` — the merging reconcile the door calls
- `hub_client` — the spoke's own client of the hub, speaking `wsp-hub-protocol` only: the TLS/TCP dial with a pinned
  CA, hello, requests and identity commands, subscribe, the sync helper and the id, lease and message verbs, the
  login file and `WSP_HUB_*` (`hubs.json`), the machine every hello names (`machine`, `machine.json`), certificate expiry, the GitHub login, and token mint/hash
- `cmd_login`, `cmd_hub`, `cmd_publish`, `cmd_allocate` — the verbs of the shared side (`wsp login`, with a token or
  `--github`, `logout`, `hub`, `publish`, `allocate`)
- `testhub` (behind `test-support`, and in this crate's tests) — a real hub for tests, run as an external program:
  `WSP_HUB_BIN` (the `wsp-hub` binary of the hub's repository) or `WSP_HUB_IMAGE` (its container image, over TLS with
  a throwaway CA, removed on drop). Seeded over the wire through `hub_client`; URL, CA, tokens, restart and stop, the
  log and data files, raw frames (`Wire`), a `Tap` that records every line; `protocol` re-exports the wire for the
  root's raw-frame tests. With neither variable, `TestHub::start` is `None` and the test returns, after one notice per
  test binary; `WSP_HUB_REQUIRED=1` makes that a failure

## Does not hold

`addon` (the interface), `door`, `mirror`, `orders`, `address`, `record_workspace`: core. They ask the add-on and
never name it.

## Tests

`cargo test -p wsp-spoke` (81). The tests that need a hub run the hub's own program (`testhub`):

```text
WSP_HUB_BIN=<path to wsp-hub> cargo test -p wsp-spoke        # the binary, a child process on loopback
WSP_HUB_IMAGE=wsp-hub:test cargo test -p wsp-spoke           # the container image, over TLS
```

Without either they are skipped, said once per test binary. Leases run out in real time (`Setup::lease_secs`); what a
test reads of the hub it reads over the wire, or from its data files. The GitHub login is tested against a scripted
connection only. They use the
engine's fixtures behind its `test-support` feature (`verification::record`, `cmd_message::tests_govern`);
the orders fixtures (`testkit::orders_db`, `orders_seat`) are its own, because the engine's normal dependencies never reach the sqlite crate. The three drive-lease scenarios are split at the add-on interface: through `cycle::step`, with a
scripted `SharedWork`, in the engine's `cycle` tests; against a real hub, asking the real `Spoke`, in `spoke.rs`. None
is left under `#[cfg(any())]`.
