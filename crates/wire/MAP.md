# wsp-wire

The `Place` port and its backends (`docs/components.md`, cut `c4`). Depends on `wsp-base`, `wsp-store` and
`serde_json`. The root re-exports each module at its old path (`crate::place`, `crate::far`, …).

## Holds

- `place` — the `Place` port, its eight verbs, `Seat`, `State`, `Refusal`, `Rect`, and `Above` (what `main` installs for `agent_commands`, `hold`, `hold_close`)
- `place_compound`, `place_super` — the two real backends; `tunnel`, `far` — the sockets and ssh they use
- `launchd`
- `fake` — the fake herdr (also served by `wsp sandbox`); it moved here with `Rect`, because the backends' own tests bind it

## Does not hold

`sync` (orchestration, remapped to seats), `memory`, `artefact`, `cmd_mandate` — still in `wsp`.

## Notes

- Unit tests run with `Above` uninstalled (kind's own name, nothing held), not the real hooks.
- `place_super` includes `../../../pi/wsp-session.ts` by path, so the crate is not packageable alone.
- `test-support` gates `StopsOnDrop`, `EndsOnDrop`, `test_sups` for the root's tests.

## Tests

`cargo test -p wsp-wire` (173 pass, 7 ignored, 6.8s of test time). `cargo test --workspace -- --list`: 2757 before and after.
