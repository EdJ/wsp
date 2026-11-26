# wsp-machines

The state core's machines (`docs/state-core.md` section 3), each a pure `Domain` (`decide`, `evolve`) over the
contract's types: section 6's crate. Depends on `wsp-store-contract` and `serde_json` (an event's payload), nothing
else, and does no IO and reads no clock (`tests/layering.rs`). The hub's identity machine and its tokens left with
the old in-tree hub: they are the hub's, in its own repository. Step 2 brings the orders machine (wsp-118); seats,
rows and lists come with steps 3 and 4. Nothing outside this crate names it yet: wsp-server composes `Orders` with
its `Mirror` at group 3 (wsp-121).

## Holds

- `src/lib.rs` — the modules, and the re-exports `Orders`, `ATTEMPTS`
- `src/orders.rs` — `Orders`, 3.1's machine: `Orders::owns(&Command)` (the seven `order.*` verbs, so a server
  routes them), `decide` with the table in its module docs, and `evolve`, which folds the `order.*` events and
  `lease.taken`/`lease.released` (`order.deliver`'s guard reads the lease). `ATTEMPTS` (3), the counted refusals
  before `failed`. Order ids are `<seat>-o<n>`, minted from the state
- `tests/orders.rs` — one test per transition of 3.1; every illegal transition in one table, each refused with
  its exact `Refusal` and nothing written (the stand-in for conformance case 9, below); each answer applied twice
  deciding nothing the second time. `Log` there is a backend's `apply` in memory: decide, stamp, fold

## Contract

- **Pure.** `decide` reads only the `State`, the command and the principal; `evolve` only the state and the
  event. A refusal drafts nothing. No `std::fs`, `std::net`, `std::process`, `std::time` or the like
  (`tests/layering.rs`).
- **The contract's refusals only** (R5: a new code is a contract row). 3.1's "no seat" is `not_found { seat }` or
  `illegal { seat, "gone", verb }`; "busy" and "not ready" are `illegal { seat, <its state>, "order.deliver" }`;
  every other illegal transition is `illegal { order, <its state>, verb }`. `order.deliver`'s fence is checked
  here too: `invalid` (another lease), `not_found` (lease), `held`, `stale`.
- **A repeat answer is an empty decision**, not a refusal: the delivery worker and the manager fold may both
  report one. `delivered` on `delivered`/`acknowledged`/`done`, `acked` on `acknowledged`/`done`, `done` on
  `done`, and `refused` on `queued`/`failed` with the same words as the order's last refusal.
- **`mirror` is refused `illegal`, state `owned`**, as the contract's MAP.md says a real domain refuses it; any
  other verb not `owns`'s is `invalid`.

## Choices beyond 3.1

- `order.cancel` on a `cancelled` order is `illegal` (no transition; case 9 refuses it so too). `failed` may be
  cancelled.
- `order.deliver` checks the order's state before the fence and the seat, so a delivery that lost its race reads
  `illegal { order, "typed" }`, not a lease refusal.
- The refusal at the cap writes `order.failed` alone (`{why, attempts}`), not `order.refused` then `order.failed`.

## Case 9

`store-conformance`'s `case_machine_guards` builds its store with `TestDomain` (`Harness::fresh`), so it cannot run
against `Orders` without a change to the suite. `tests/orders.rs`'s
`illegal_transitions_are_refused_with_their_name_and_write_nothing` stands in for it: the same guarantee (exact
named refusal, the head and the state unchanged), through `Log`.
