# wsp-trees

Worktrees, build trees and the throttled cargo (`docs/components.md`, cut `c6`). Depends on `wsp-base`, `wsp-store`,
`wsp-wire`. The root re-exports each module at its old path (`crate::cmd_checkout`, `crate::throttle`, …).

## Holds

`cmd_checkout`, `cmd_verify`, `treecopy`, `check_tree`, `throttle`, `warm`, `overlap`, `guard`, `greens`, `at_stop`, `gc`
(the `trees` rows of the layering map).

## Does not hold

`build_output`, `leg_tree`, `cold`, `affected` — the layering map puts them in `engine`; they stay in `wsp`. `disk` stays in base's row.

## Notes

- `test-support` gates `Greens::record` and `throttle::sample_pause` for the root's tests.
- Tests that wrote a barrier check use `checks::push` directly.

## Tests

`cargo test -p wsp-trees`.
