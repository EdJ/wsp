# wsp-base

The bottom of the stack (`docs/components.md`, cut `c1`). Depends on `serde_json` and nothing in the
workspace; owns no state. Every other component may use it, and it uses none of them.

## Holds

- `src/util.rs` — paths, time, ids, `Paint`, `Clock`/`Dial`, env guards (`env_lock`, `isolated`), small string helpers
- `src/calls.rs` — what a wsp call costs (`wsp-481`): the `Command` every spawn goes through, the `calls.jsonl` record and its reader (`docs/calls.md`)
- `src/fm.rs` — the YAML-frontmatter subset
- `src/input.rs` — key and line input parsing
- `src/risk.rs` — command risk rules (`RULES`)
- `src/choices.rs` — the choice/question JSON shapes
- `src/base/args.rs` — `Args`, the flag tables (`BOOL_FLAGS`), `Args::synth`
- `src/base/git.rs` — `git`, `git_ok`, `toplevel`
- `src/base/labels.rs`, `src/base/term.rs` — glyph table, `stty`
- `src/base/seat.rs` — `SEAT_ENV`

## Contract

Everything is `pub`; the root package re-exports each module at the path it had (`crate::util`,
`crate::base::args`, …), so no caller changed. Test-only helpers in `util` and `risk` (`ENV`,
`Dial`, `Paint::painted`, `isolated`, …) sit behind the `test-support` feature, which the root
package turns on for its dev build only.

## Does not hold

- the build stamp (`build_stamp`, `stamp_word`) — `src/base/stamp.rs` in `wsp`, because `build.rs` writes `WSP_COMMIT`
- the store, `Task`, `Project` — `crates/store` (`wsp-store`)
- the git repo test fixture — `crates/testkit`; the store-backed fixture — `crates/store/src/testkit.rs`
- `disk`, `memory` — still in `wsp`; each reaches up (`cmd_checkout`, `place`)

## Tests

`cargo test -p wsp-base` (47 tests, under a second). Whole workspace: `cargo test --workspace`.

## Files that change most

`src/util.rs` (35 of the last 60 commits touching this crate), then `src/input.rs`.
