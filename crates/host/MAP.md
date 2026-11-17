# wsp-host

The in-process host (`docs/agent-manager.md` section 6.2, M1 group 1; wsp-020): every OS operation a seat
needs, run in this process. Depends on `wsp-base`, `wsp-host-api` and `serde_json`, and on nothing of
`wsp-store`, `wsp-store-contract` or `wsp-wire`, so the agent manager links it (2.3, R1; wsp-021). `tests/layering.rs` (`the_host_crate_links_neither_the_store_nor_the_wire`)
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
- `src/tcp.rs`: `Tcp`, the TCP carrier of `wsp-host-api`'s `Connection` (3.4). It frames newline-delimited
  JSON with a 16 MB line cap and a write timeout of three beats, and offers `dial`, `try_clone` (a second handle
  to send on), `recv_line` (a line undecoded, for the manager's first) and `shutdown`. The manager serves on it, and wire's client (wsp-025) will dial with it
- `src/facts.rs`: the beat's host facts (`read`, `assemble`) turned into `wsp-host-api`'s `HostFacts`, with
  their parsers. Three asks of `InProcess` per reading: `sysctls` (load, swap, the kernel's memory pressure
  level), `disks_free` (one `df` for every volume) and `thermal`
- `src/hooks.rs`: the manager's hooks endpoint (wsp-023). `report` is the hook command's half: under
  `WSP_HOST=manager` it dials the manager on loopback (`WSP_MANAGER_ADDR`, default `127.0.0.1:7487`), sends one
  `hook.post` line with the token read from `<exec_dir>/manager.token` (`WSP_MANAGER_DIR`), and reads one
  `hook.taken` or `hook.refused` line; anything else is `Sent::Unanswered`, and wire's `report_to` writes the
  hook log instead. `Fold` is the manager's half: `UserPromptSubmit` is `turn began` (with the order
  `awaiting` named), `PostToolUse` is `seat.spent` (the burn tally), and `Stop` is `seat.spent`, `seat.said`,
  `seat.did` (`turn_of`'s digest, the transcript's `(host, path)`) and `turn ended`. `a_childs` moved down from
  `place_super`. The post is a host-local line beside 3.2's frames, not a `Message`: no M1 group changes
  `wsp-host-api`
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

`tcp` and `facts` have their own unit tests (`cargo test -p wsp-host`). The rest of this crate's code is tested where its callers are: `cargo test -p wsp-wire` (`place_compound`,
`place_super`) and the root's `cmd_spawn::`. `cargo build -p wsp-host` alone shows that it links no store.
