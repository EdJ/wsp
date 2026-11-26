# wsp-engine

The engine and the seats, the phase 2 cut (`docs/components.md` §5, wsp-440). Depends on `wsp-base`, `wsp-store`,
`wsp-wire`, `wsp-trees` and the state core's crates (`wsp-store-contract`, `wsp-api`, `wsp-client`, `wsp-server`,
`wsp-view`, `wsp-machines`, `wsp-objects`, `wsp-host-api`). The root re-exports each module at its old path
(`crate::cycle`, `crate::cmd_spawn`, …), so no caller changed.

## Holds

The `engine` and `seats` rows of the layering map, 97 modules: the run (`worklist`, `cycle`, `repair`, `verification`,
`legs`, `leg_seat`, `stage`, `barrier*`, `check`, `closing`, `ending`, `hold*`), the row and list verbs (`cmd_task`,
`cmd_worklist`, `cmd_message`, `cmd_submit`, …), orders, `fold`, `mirror`, `deliver`, the hub's spoke side
(`hub_*`, `cmd_hub`, `cmd_login`), and the seat commands (`cmd_spawn`, `cmd_agent`, `agent_commands`, `wake`, `room`,
`cmd_govern`, `cmd_watch`, `cmd_machine`, `cmd_sandbox`, `cmd_install`, …), with `disk` and `memory`. The rows stay
two in the map so a later split starts from them; the 28 `engine -> seats` lines are a ratchet inside the crate.

## Does not hold

`timeline`, `surface` and `top` (`cmd_report`, `cmd_cost`, `cmd_attempts`, `panel`, `daemon`, `main`, `registry`, …):
they stay in `wsp` and use this crate, never the other way (`docs/components-upward.txt` has no line out of it).

## Hooks the binary fills

- `base::stamp::BUILD_STAMP`: the root's `build.rs` writes the commit, so `main` hands `build_stamp` in. Unset it is
  empty; in this crate's own tests it is a fixed `0000000+dirty`, a stamp they can tell from none.
- `outputs::install(cmd_report::REPORTS)`: timeline's report writers. This crate's tests install stand-ins that put
  `effort.*` and `work.html` where the real ones do; the real writers are tested in `cmd_report`.
- `cycle::install_seats(&cmd_spawn::Fleet)`: the engine reaches seats only through `cycle::seats()`, the installed
  `dyn Seats` (wsp-542). `main` installs first thing; a call before that panics. In a test the install is that thread's
  own, and with none it reads `cycle::install_tests::Double`, which records `say` (`SAID_TO`), answers only what a test
  scripted (`OWED`, `ANSWERS`) and defaults the rest: assert no other effect through it.
- `mirror::LIST_OUTPUTS`, `LIST_SHOW`, `SEAT_WORKING`, `ROW_HOLDER`, `cmd_checkout::install_engine`: as before the cut.

## Notes

- Every `pub(crate)` became `pub` in the move.
- `test-support` (wsp-544) gives a dependent's tests the fixtures and recorders they name; `tests/dependent.rs` lists
  them. Guards on `base::under_test()` (`start_detached`'s among them) hold here and, with the feature, in a test
  process run from `target/*/deps`, so in the root's unit tests too; not in the `wsp` binary its integration tests run.
- A guard still on `cfg!(test)` (`cycle::start`, `start_check`, `trunk_tip`, `tell`, `tell_logged`, ...) holds only in
  this crate's tests: a dependent's test that reaches one takes the real path.
- A dependent installs the `state.db` open (`orders::tests::install_db`) and takes `testkit::repo_at` from its own
  `wsp-testkit`: layering keeps both out of this crate's normal dependencies.
- Paths: `include_str!` reaches the repository root as `../../../`; tests that read files do it from
  `env!("CARGO_MANIFEST_DIR")/../..`.
- `doctest = false`: the doc blocks are prose, never run as doctests while these modules were the bin's.
- A filter for one of these modules needs the package: `cargo test -p wsp-engine cycle::`. A bare `cargo test
  cycle::` runs the root package and finds nothing.

## Tests

`cargo test -p wsp-engine` (about 2300). `wsp-store-sqlite` is a dev-dependency only: orders, fold and mirror tests
open a real `state.db`. `cargo test --test layering` holds the crate's links and its upward lines.
