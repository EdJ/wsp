# wsp-engine

The engine and the seats, the phase 2 cut (`docs/components.md` §5, wsp-440). Depends on `wsp-base`, `wsp-store`,
`wsp-wire`, `wsp-trees` and the state core's crates (`wsp-store-contract`, `wsp-api`, `wsp-client`, `wsp-server`,
`wsp-view`, `wsp-machines`, `wsp-objects`, `wsp-host-api`). The root re-exports each module at its old path
(`crate::cycle`, `crate::cmd_spawn`, …), so no caller changed.

## Holds

The `engine` and `seats` rows of the layering map, 100 modules: the run (`worklist`, `cycle`, `repair`, `verification`,
`legs`, `leg_seat`, `stage`, `barrier*`, `check`, `closing`, `ending`, `hold*`), the row and list verbs (`cmd_task`,
`cmd_worklist`, `cmd_message`, `cmd_submit`, …), orders, `fold`, `mirror`, `deliver`, the shared-work add-on
interface (`addon`), the hub's spoke side behind it (`spoke`, `hub_*` with the on/off switch `hub_switch`, `cmd_hub`, `cmd_login`, `cmd_publish`,
`cmd_allocate`, `reconcile`: no other module names these), and the seat commands (`cmd_spawn`, `cmd_agent`, `agent_commands`, `wake`, `room`,
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
- `mirror::LIST_OUTPUTS`, `LIST_SHOW`, `SEAT_WORKING`, `ROW_HOLDER`, `cmd_checkout::install_engine`: as before the cut.
- `addon::install(Arc::new(spoke::Spoke::new()))`: the shared-work add-on, which core asks through `addon::get` and
  never by its modules. Unset, every answer is the standalone one. The daemon starts its background work with one
  `addon::get().start(store, Lent { .. })`. This crate's tests run standalone unless a test puts an add-on on its
  own thread with `addon::scoped` (the spoke's tests put the spoke there).

## Notes

- Every `pub(crate)` became `pub` in the move. `#[cfg(test)]` and `cfg!(test)` guards (spawns, ends, tells, launches,
  the test door) hold only in this crate's tests: the root's tests run the engine as the binary does. They set
  `WSP_NO_CYCLE` where a verb would launch, and serve an orders store in process (`orders::serve_in_process`).
- Paths: `include_str!` reaches the repository root as `../../../`; tests that read files do it from
  `env!("CARGO_MANIFEST_DIR")/../..`.
- `doctest = false`: the doc blocks are prose, never run as doctests while these modules were the bin's.
- A filter for one of these modules needs the package: `cargo test -p wsp-engine cycle::`. A bare `cargo test
  cycle::` runs the root package and finds nothing.

## Tests

`cargo test -p wsp-engine` (about 2290). `wsp-store-sqlite` is a dev-dependency only: orders, fold and mirror tests
open a real `state.db`. `cargo test --test layering` holds the crate's links and its upward lines.
