# wsp-store

The records (`docs/components.md`, cut `c5`). Depends on `wsp-base` and `serde_json`, nothing else in the
workspace; zero upward edges in the layering test. It is the one writer of the task, project, worklist,
message and machine records and of `~/.local/state/wsp`. The root package re-exports each module at the
path it had (`crate::store`, `crate::model`, …), so no caller changed.

## Holds

- `src/model.rs` — `Task`, `Project`, `Worklist`, `Machine`, `Status`, the section accessors
- `src/store.rs` — `Store` (open, read, save, lock), claims, the task-head index, `log_line`
- `src/message.rs`, `src/lapse.rs` — messages between seats and the person; closing the ones a row's end lapses
- `src/resolve.rs` — id and name resolution
- `src/checks.rs`, `src/caption.rs`, `src/sharing.rs` — check records, caption rules, build-share budgets
- `src/governors.rs`, `src/agent_kind.rs` — governor addressing, the kind of agent a row runs
- `src/log.rs`, `src/decisions.rs`, `src/tags.rs`, `src/verbs.rs` — the `## Log`, `## Decisions` and tag writers; the verb list
- `src/testkit.rs` — `store(tag)` and `isolated_store(tag)`; behind `test-support`

## Contract

Everything is `pub`. The constants it exports (`SCHEMA`, `PROSE`, `SECTIONS`, `DEFAULT_LEASE`, …) are defined in `crates/store-contract` (R2) and re-exported from `model`; the types stay here because their inherent methods must live in their defining crate. `store::BUILD_STAMP` is set once by the binary (`main`) because `build.rs` writes the
stamp's variables for the root package only; the task-head index keys on it. Test-only helpers and the
guard that refuses a test opening the live store (`Store::open`, `log_line`'s echo) are on under
`cfg(any(test, feature = "test-support"))`, which the root package turns on for its dev build only.

## Does not hold

- `scratch`, `cmd_migrate` — still in `wsp`: each reaches up (`disk`, `verification::migrate`, `barrier::migrate`)
- `artefact` (wire), `kind_data`, `cmd_project` (seats), `cmd_mandate` (wire) — assigned to those components in the layering table
- the git repo fixture — `crates/testkit`. The store-backed fixture is here, not there: a `wsp-testkit` that depends on `wsp-store` would hand `wsp-store`'s own unit tests a second copy of `Store`

## Tests

`cargo test -p wsp-store` (169 tests, 12s of test time, 21s wall with the build). Whole workspace: `cargo test --workspace`.
