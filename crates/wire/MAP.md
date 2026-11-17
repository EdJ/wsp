# wsp-wire

The `Place` port and its backends (`docs/components.md`, cut `c4`). Depends on `wsp-base`, `wsp-host`, `wsp-host-api` (the manager's `hello`), `wsp-store`
and `serde_json`. The root re-exports each module at its old path (`crate::place`, `crate::far`, …).

## Holds

- `place` — the `Place` port, its eight verbs, `Seat`, `State`, `Refusal`, `Rect`, and `Above` (what `main` installs for `agent_commands`, `hold`, `hold_close`)
- `place_compound`, `place_super` — the two real backends; `tunnel`, `far` — the sockets and ssh they use
- `host` — the `Host` trait, `LOCAL`, and the adapter: `host::InProcess` wraps `wsp-host`'s, each method calling
  the one of the same name; everything else of `wsp_host::inproc` is re-exported here (wsp-020)
- `host_tcp` — `host::Tcp`, the second `Host`: each moved method as its 3.2 message to the manager the local
  record names, the rest in-process; `WSP_HOST=inproc|manager` (`Choice`, `chosen`; default `manager` since wsp-029); "host
  silent" when no `hello` comes back within 1 s; `Subscription`, `subscribe {from}` with the last id kept per
  host in `<state>/manager/<host>.last` (wsp-025). `tests/host.rs` holds both implementations to one contract
- `launchd` — the daemon's agent and the manager's (`<label>.manager`, wsp-024)
- `manager` — wsp's side of the manager on this machine: the local record and secret file `wsp install` writes, and `doctor`'s hello probe
- `fake` — the fake backend (also served by `wsp sandbox`); it moved here with `Rect`, because the backends' own tests bind it

## Does not hold

- the in-process host's OS code, and the `Seat`/`State`/`Refusal`/`Delivery` words, the shed list and the prices
  it needs — `crates/host` (`wsp-host`), re-exported at their old paths in `host`, `place`, `place_super` and
  `place_compound`
`sync` (orchestration, remapped to seats), `memory`, `artefact`, `cmd_mandate` — still in `wsp`.

## Notes

- Unit tests run with `Above` uninstalled (kind's own name, nothing held), not the real hooks.
- `place_super` includes `../../../pi/wsp-session.ts` by path, so the crate is not packageable alone.
- `test-support` gates `StopsOnDrop`, `EndsOnDrop`, `test_sups` for the root's tests, and turns on
  `wsp-host/test-support` (`test_sups` and `RENDER` live there now).

## Tests

`cargo test -p wsp-wire` (171 pass, 7 ignored, 5.5s of test time, 2026-11-17 after wsp-020's move; the same 178 `#[test]` before it). `cargo test --workspace -- --list`: 2757 before and after.
