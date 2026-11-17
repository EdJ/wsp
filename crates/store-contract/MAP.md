# wsp-store-contract

What the other components may name of the records without depending on a store (`docs/components.md` R2, cut
`c5`). A contract crate: types, traits and constants, no logic and no IO. Depends on `serde` (derive) and
`serde_json` (an event's payload), nothing in the workspace. `wsp-store` uses the constants; nothing uses the
state core's types yet (wsp-046): the conformance suite (wsp-047) and `wsp-store-sqlite` (step 1) will.

## Holds

- `src/lib.rs` — today's file store's constants (`SCHEMA`, the section lists, `DEFAULT_LEASE`, the policy words),
  unchanged; the modules below, re-exported at the root
- `src/id.rs` — 2.1's keys and 2.2's values: `EventId` (monotonic), `IdempotencyKey`, `Token` (per lease name),
  `Rev`, `At` (Unix ms), the ids (`RowId`, `ListId`, `GroupId`, `SeatId`, `OrderId`, `AskId`, `MachineName`,
  `LeaseName`, `ProjectId`, `RunKey`), and `Principal` (person, seat, manager, executor, `wsp:<instance>`)
- `src/entity.rs` — 2.1's entities with section 3's states as enums: `Row`/`RowState` (3.3), `List`, `Group` and
  their shared `ListState` (3.4), `Seat`/`SeatState` (3.2), `Order`/`OrderState` (3.1), `Claim`, `Ask` (kind
  ask/note/flag), `Run`, `Machine`/`Transport`, `Lease`; `State`, the whole domain as `snapshot()` returns it
- `src/event.rs` — `Event` (2.1: id, at, principal, kind, subject, payload), `EventKind` (the kinds section 3's
  machines write, by wire name: `seat.started`, `order.acknowledged`, ...), `Subject`, and `Draft` (an event
  before the backend stamps its id and moment)
- `src/command.rs` — `Command`, one variant per command verb of 4.2, tagged `verb` with `args` as 4.1's envelope
  carries them; `Fence { lease, token }` (2.2) and `FENCED_VERBS`; `Applied` (the new events' ids); `Block`,
  `Verdict`, `ReleaseTo`
- `src/refusal.rs` — `Refusal`, tagged `code`: `stale` (a fence), `too_old` (4.3), `conflict` (a text rev),
  `held`, `not_found`, `illegal` (a guard of section 3), `forbidden`, `invalid`, and `unknown` for a newer
  peer's code; `Error` (refused, or the backend failed) and `Result`
- `src/domain.rs` — `Domain`: `decide` and `evolve`, the two signatures `apply` validates through (below)
- `src/store.rs` — 2.2's trait family: `Reads`, `Events` (`events_after`, `head`, `wait_for`), `Commands`
  (`apply`), `Leases`, `Claims`, `Texts`, and `Store`, all six, blanket-implemented; `RowFilter`, `SeatFilter`;
  `STORE_INSTANCE`; the module docs' table of the events a store writes itself
- `src/subscribe.rs` — 4.3: `Subscribe { from? }`, `Feed` (a snapshot, an event, a beat), `BEAT_SECS`,
  `MISSED_BEATS`
- `tests/round_trip.rs` — every command, event kind, entity state and refusal round-tripped under its wire name,
  each table's coverage held by an exhaustive match; `FENCED_VERBS` held to the variants carrying `args.fence`; one
  `State` of every entity; a trivial `Store` and `Domain`

## Contract

Every change is a change to the contract (R5) and is a row of its own. A field added later is `#[serde(default)]`;
an event kind or refusal code added later decodes as `Unknown` on an older peer. No SQL, connection, transaction or
backend row type appears here (D1), and no tenant field: isolation is the instance (8.6).

**The traits are object-safe**, not generic: `&self` methods, no generic methods, nothing returns `Self`, so a
backend is held as `&dyn Store` or `Box<dyn Store>`, and a domain as `&dyn Domain`. The suite may still be generic
over `impl Store`. `Reads::claim` and `Claims::claim`, and `Leases::release` and `Claims::release`, share names as
2.2 writes them, so a caller holding a whole `Store` names the trait (`Claims::release(&s, row, token)`).

## Choices beyond section 2

- **Validation (wsp-043's decision (a), as recommended).** A backend's `apply` runs `Domain::decide` on its
  state, stamps the `Draft`s it returns with ids, its clock and the principal, and writes them in one atomic write;
  its state is the fold of `Domain::evolve`. `decide` takes the principal beside the command, since section 3's
  guards name who asks ("the requester holds the claim or is the person").
- `Principal` and `Subject` serialize in serde's derived form (`{"person":"ed"}`); the `person:ed` text form is a
  rendering, left to the API (step 1), since parsing it would be logic here.
- `ListState` is one enum for lists and groups, as 3.4's table is one column.
- `unblock` is `row.block` with `Block::Off`, and the verifier's holds/blocks is `row.land`'s `Verdict`, so the
  commands stay the 19 verbs of 4.2. Executor reports (section 5: `started`, `delivered`, `acknowledged`, a run's
  verdict) are not commands yet; they join at step 2 by a row of their own.
- `Claims::claim` takes `Option<&Fence>`: 2.2 names a fence but not whose lease it is, and a person's claim has none.
- `Project` has no entity: rows carry a `ProjectId`, and the project's record stays in files (2.5).
- `Error::Backend` beside `Refusal`: a backend that cannot reach its file or server is no refusal, and is retried.

## What a backend and a domain must agree on (wsp-065)

Found by wsp-047's MemStore; every backend picks these answers, or 2.3 case 6 (the snapshot is the fold) diverges.

- **The events a store writes itself.** `Leases`, `Claims` and `Texts::set_text` change the state with no command,
  so the backend writes their events in the same atomic step: `take` → `lease.taken` carrying the `Lease`;
  `release` → `lease.released`, payload `null`; `Claims::claim` → `claim.taken` carrying the `Claim`;
  `Claims::release` → `claim.released`, payload `null`; subjects `Lease(name)` and `Claim(row)`, by the holder.
  **`renew` writes `lease.taken` again** with the same token and the new `expires` (no `lease.renewed`): `evolve`
  reads `taken` as "the lease is now this", so `expires` stays in the fold. A claim's lease writes `claim.*` only.
  A call that changes nothing writes nothing. The table is in `src/store.rs`'s module docs.
- **`set_text`'s principal.** It takes none, so `text.set` (subject `Row(row)`, payload `{"section", "rev"}`) is by
  `Principal::Wsp(STORE_INSTANCE)`, `wsp:store`. The text stays out of the event and the state (2.5).
- **A command's fence.** No accessor (logic, R2): a fenced verb's fence is the argument named `fence`, and only a
  fenced verb has one, so a command is fenced when `args.fence` is present and not `null`. `FENCED_VERBS` lists
  them (`row.claim`, `row.review`, `row.land`, `seat.end`); `tests/round_trip.rs` holds the table to the variants,
  so a fifth fenced verb fails there until it is listed. `apply` checks the fence before `decide`.
- **Idempotency's first result.** The key is the identity: a repeated key returns the first `Applied`, even with a
  different command, which is not decided. A refusal is no result and records no key, so a retry decides again.
