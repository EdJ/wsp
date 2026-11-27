# wsp-seats

The seats, the second cut of the engine crate (`docs/components.md` §5, wsp-441). Depends on `wsp-engine` and the
members it names directly (`wsp-base`, `wsp-store`, `wsp-wire`, `wsp-host-api`, `wsp-store-contract`, `wsp-server`,
`wsp-api`, `wsp-client`, `wsp-view`). `wsp-engine` never depends on it.

## Holds

The `seats` row of the layering map, 19 modules: `cmd_spawn` (with `Fleet`, the engine's `Seats` impl), `cmd_agent`,
`agent_commands`, `wake`, `cmd_govern`, `cmd_watch`, `cmd_machine`, `cmd_sandbox`, `cmd_install`, `cmd_brief`,
`cmd_resume`, `cmd_role`, `cmd_project`, `cmd_stamp`, `cmd_attend`, `kind_attention`, `kind_data`, `memory`, `seat_block` (wsp-788:
a seat blocked at a question raised to the person in yours; it uses `cmd_agent` and `cmd_spawn`, so it sits here,
not in the engine where master first put it).

## Does not hold

- `room`: it stays in `wsp-engine`, since `cycle` and `waits` use `room::Gate` in product code.
- `watch_levels`, `claims`, `prose`, `sent`: moved down into the engine by wsp-438, used from here.
- Everything else of the run and the row verbs: `wsp-engine`.
- The shared-work add-on (the hub's spoke: leases, the drive, the sync, `wsp login`/`hub`/`publish`/`allocate`):
  `wsp-spoke`, never named here. Seats code asks it through the engine's `addon` (`crate::addon::get()`), as the
  engine does: `cmd_agent`'s lease on a claim and the doctor's hub lines, `cmd_spawn`'s allocation refusal, `wake`'s
  primary of another user, `cmd_sandbox`'s isolated login.

## Paths

- `lib.rs` re-exports every `wsp-engine` module (and the ones engine re-exports from `wsp-base`, `wsp-store`,
  `wsp-trees`, `wsp-wire`) at its old path, so `crate::cycle`, `crate::store`, … read as they did. The root re-exports
  these 18 at theirs (`main.rs`), so no caller changed.
- The engine reaches the seats only through `cycle::seats()`: the root installs `&cmd_spawn::Fleet` with
  `cycle::install_seats` first thing (wsp-542). A test here that needs the real seats installs it too.
- `include_str!` reaches the repository root as `../../../`; tests that read files do it from
  `env!("CARGO_MANIFEST_DIR")/../..`.

## Tests

`cargo nextest run -p wsp-seats <module>::` (756 tests; `cargo test -p wsp-seats` where nextest is not installed).
A bare `cargo test wake::` runs the root package and finds nothing.

- Dev-dependencies: `wsp-engine` with `test-support` (its fixtures and recorders, `base::under_test`, wsp-544),
  `wsp-testkit` (`testkit::{git, repo, repo_at}`), `wsp-manager` and `wsp-host` (the hook tests), and
  `wsp-store-sqlite`, for tests alone (d1 on wsp-441).
- A test that opens a real `state.db` calls `crate::install_db()` first: engine's `orders::tests::open_db` takes the
  open from a dependent, since layering keeps the sqlite crate out of engine's normal dependencies.
- Engine's guards on `cfg!(test)` (`cycle::start`, `start_check`, `trunk_tip`, …) are off here: a test that reaches
  one takes the real path. `cycle::tell` records into `cycle::tests::TOLD` here as well, so "nobody was told" holds.
- `cargo test --test layering` holds this crate's links (no sqlite crate, hub or binary through a normal dependency)
  and walks its source for upward lines.

## Files that change most

Commits since 25 Oct (`git log`, old paths included): `cmd_agent` 155, `cmd_spawn` 125, `cmd_watch` 37, `wake` 30,
`cmd_brief` 27, `cmd_role` 26, `cmd_machine` 25, `cmd_install` 22. A seats-only edit narrows `wsp check --affected`
to `-p wsp-seats` and the root package (`affected::reach`). `doctest = false`: the doc blocks are prose.
