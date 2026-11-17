# wsp-host

The in-process host (`docs/agent-manager.md` section 6.2, M1 group 1; wsp-020): every OS operation a seat
needs, run in this process. Depends on `wsp-base` and `serde_json`, and on nothing of `wsp-store`,
`wsp-store-contract` or `wsp-wire`, so the agent manager can link it (2.3, R1). `wsp-host-api` joins when the
manager's messages are spoken here. `tests/layering.rs` (`the_host_crate_links_neither_the_store_nor_the_wire`)
holds the rule; it walks the path dependencies, so a dependency reached through another crate counts too.

## Holds

- `src/inproc.rs` — `InProcess` and its methods (the bodies of wire's `host::Host`, one per method, same
  names); the spawn and stop types (`Spawn`, `Session`, `Headless`, `Forked`, `Stop`); the hook files
  (`SAID_FILE`, `HOOKS_FILE`, `BURN_FILE`, `SPENT_FILE`); and the free functions they are built from: the
  `ps`/`kill`/`lsof` work (`tree_of`, `signal_group`, `one_ps`), `compound-sup` and `compound-render`
  (`find_sup`, `compound_render_binary`, `sup_command`, `deliver_with`, `type_and_submit`, the handshake and
  the wire), the burn tally (`tally_burn`, `bank_burn`, `burn_under`), and the transcript readers
  (`claude_transcript`, `pi_session_file`, `opencode_rows`, `captured`). Also `WIRE_VERSION`,
  `DELIVERY_PENDING`, `reads_kind`, `read_json` and `str_of`, moved down with the code that uses them
- `src/port.rs` — `Seat`, `State`, `Refusal`, `Delivery`, `Result`: the words the methods answer in
- `src/shed.rs` — what a spawned seat must not inherit (`CHILD_MARKER`, `shed`, `KEPT`, `PI_SESSION`, `shed_keys`)
- `src/price.rs` — the price table and `cost`/`cost_of`, which the burn tally prices each request with
- `src/test_sups.rs` — the supervisors a test starts, tied to the test process (wsp-432); `test-support` only

## Contract

Everything here was in `wsp-wire` until wsp-020, and the wire re-exports each item at its old path:
`host::*` for `inproc`, `place::{Seat, State, Refusal, Delivery, Result, shed, …}`,
`place_super::{cost, PRICES, read_json, …}`, `place_compound::{WIRE_VERSION, reads_kind, test_sups, …}`.
No caller changed. The `Host` trait is the wire's: its `host::InProcess` wraps this one, so callers still
name the trait to ask it anything.

## The guard

`tests/host_seam.rs` allows the OS probes (`ps`, `kill`, `lsof`, the fact probes, `compound-render`, the
transcript and hook-log paths) under `crates/host/src/` and nowhere else, and fails a caller outside that
names one of the free functions above past the trait (`wsp_host::inproc::tree_of`, `host::signal_group`, …).

## Features

`test-support` gates `test_sups` and `RENDER`, a per-thread stand-in `compound-render`. `wsp-wire`'s own
`test-support` turns it on, and the wire's dev build turns it on for the wire's own tests.

## Tests

This crate's code is tested where its callers are: `cargo test -p wsp-wire` (`place_compound`,
`place_super`) and the root's `cmd_spawn::`. `cargo build -p wsp-host` alone shows that it links no store.
