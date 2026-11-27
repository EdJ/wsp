# wsp-engine

The engine, the phase 2 cut (`docs/components.md` §5, wsp-440; the seats left for `wsp-seats` at wsp-441, the hub's
spoke side for `wsp-spoke`). Depends on `wsp-base`, `wsp-store`, `wsp-wire`, `wsp-trees` and the state core's crates
(`wsp-store-contract`, `wsp-api`, `wsp-client`, `wsp-server`, `wsp-view`, `wsp-objects`, `wsp-host-api`). The root
re-exports each module at its old path (`crate::cycle`, `crate::cmd_task`, …), so no caller changed.

## Holds

The `engine` row of the layering map, 80 modules: the run (`worklist`, `cycle`, `repair`, `verification`, `legs`,
`leg_seat`, `stage`, `barrier*`, `check`, `closing`, `ending`, `hold*`), the row and list verbs (`cmd_task`,
`cmd_worklist`, `cmd_message`, `cmd_submit`, …), orders, `fold`, `mirror`, `deliver`, the import door (`door`), the
shared-work add-on interface (`addon`), which the add-on fills (it is in `wsp-spoke`, which depends on this crate and
is never named here), `room` (cycle and waits use `room::Gate`), `watch_levels`, `sent`, `claims`, `prose`, `disk`.

## Does not hold

The seats (`cmd_spawn`, `cmd_agent`, `agent_commands`, `wake`, `cmd_govern`, `cmd_watch`, `memory`, …): `wsp-seats`,
built on this crate, which never names it (`tests/layering.rs`); see `crates/seats/MAP.md`.
The hub's spoke side (`hub_*`, `cmd_hub`, `cmd_login`, `cmd_publish`, `cmd_allocate`, `reconcile`, `spoke`):
`wsp-spoke`, built on this crate and reached only through `addon` (`tests/layering.rs`).
`timeline`, `surface` and `top` (`cmd_report`, `panel`, `daemon`, `main`, …) stay in `wsp`. Neither reaches back
(`docs/components-upward.txt` has no line out of it).

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
- `addon::install(Arc::new(wsp_spoke::spoke::Spoke::new()))`, in the root's `main`: the shared-work add-on (crate
  `wsp-spoke`), which core asks through `addon::get` and never by its modules. Unset, every answer is the standalone one. The daemon starts its background work with one
  `addon::get().start(store, Lent { .. })`. This crate's tests run standalone unless a test puts an add-on on its
  own thread with `addon::scoped`. Behind `test-support` this crate exports the fixtures the spoke's tests use:
  `verification::record`, `cmd_message::tests_govern`.

## Notes

- Every `pub(crate)` became `pub` in the move (wsp-440). Of those written since, the 12 `wsp-seats` names
  (`watch_levels`, `sent`, `stage`) became `pub` at wsp-441; the rest are used inside this crate alone.
- `test-support` (wsp-544) gives a dependent's tests the fixtures and recorders they name; `tests/dependent.rs` lists
  them. Guards on `base::under_test()` (`start_detached`'s among them) hold here and, with the feature, in a test
  process run from `target/*/deps`, so in the root's unit tests too; not in the `wsp` binary its integration tests run.
- A guard still on `cfg!(test)` (`cycle::start`, `start_check`, `trunk_tip`, `tell`, `tell_logged`, ...) holds only in
  this crate's tests: a dependent's test that reaches one takes the real path. `tell` and `tell_logged` record into
  `cycle::tests::TOLD` there as well (wsp-441), so a seats test's "nobody was told" can fail.
- A dependent installs the `state.db` open (`orders::tests::install_db`) and takes `testkit::repo_at` from its own
  `wsp-testkit`: layering keeps both out of this crate's normal dependencies.
- Paths: `include_str!` reaches the repository root as `../../../`; tests that read files do it from
  `env!("CARGO_MANIFEST_DIR")/../..`.
- `doctest = false`: the doc blocks are prose, never run as doctests while these modules were the bin's.
- A filter for one of these modules needs the package: `cargo test -p wsp-engine cycle::`. A bare `cargo test
  cycle::` runs the root package and finds nothing.

## Tests

`cargo test -p wsp-engine` (1548 tests, 1 of them ignored, at abf65ef; `tests/dependent.rs` adds one where
`test-support` is on; the seats' tests are counted in `crates/seats/MAP.md`). `wsp-store-sqlite` is a dev-dependency
only: orders, fold and mirror tests open a real `state.db`. `cargo test --test layering` holds the crate's links and
its upward lines.
