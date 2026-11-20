# wsp-store-conformance

The state store's conformance suite (`docs/state-core.md` 2.3), and `MemStore`, the in-memory fake it first runs
against. A backend is done when it passes every case, with no exceptions file. Depends on `wsp-store-contract`
(and `serde_json`, an event's payload) and nothing else of the workspace; `wsp-testkit` may join when a case needs
it (section 6; `tests/layering.rs`). Nothing in wsp uses it yet; `wsp-store-sqlite`'s tests will at step 1.

## Holds

- `src/lib.rs` — the modules, and the re-exports `Harness`, `MemHarness`, `MemStore`, `Knobs`, `TestDomain`
- `src/harness.rs` — `Harness`: what a backend implements for the suite. `type Handle: Store + Send + Sync`;
  `create(domain)` a new store, `open(&h)` another handle on `h`'s store, `crash_next_apply(&h)` the test hook,
  `reopen(&h)` after a crash; `handles(n)` and `fresh()` with the test domain. `MemHarness { knobs }` for MemStore
- `src/mem.rs` — `MemStore` (public): one `Arc` per store shared by its handles, one mutex over the event log,
  the folded state, the idempotency keys, the lease table and the texts, and a condvar for `wait_for`.
  `crash_next_apply`/`reopen` (case 8), `Knobs` (the bite tests' four breakages), `fence_of(&Command)`. The
  module docs say what it writes for leases, claims and texts
- `src/domain.rs` — `TestDomain`: rows, seats, claims and orders, with the guards case 9 drives (the table in
  its module docs); `claim_lease`, `verify_lease`, `verb`. It refuses step 1's `mirror` as `illegal` (state
  `owned`), as a real domain does (wsp-070; the contract's MAP.md), and a unit test holds it
- `src/cases/` — the ten cases, each `pub fn case_x<H: Harness>(h: &H) -> CaseResult`, one file each (wsp-047's legs
  `state` and `events`); `mod.rs` holds `CaseFailure` (case number, name, what broke) and the setup helpers:
  `case_atomicity` (1, `atomicity.rs`), `case_idempotency` (2), `case_lease_exclusivity` (3, `leases.rs`),
  `case_fencing` (4), `case_event_order` (5, `order.rs`), `case_snapshot` (6), `case_wakeups` (7), `case_crash`
  (8), `case_machine_guards` (9, `guards.rs`, on `TestDomain` until `wsp-machines` exists), `case_text` (10). Each
  file's module docs say what the case drives
- `tests/mem.rs` — smoke tests (a command through MemStore read back as its event; the fenced claim path; the
  crash hook); each case against MemStore, one `#[test]` per case (`mod cases`); and the bite tests (`mod bites`),
  one per knob: `ignore_idempotency` fails case 2, `skip_fence` 4, `gap_in_ids` 5, `write_half` 8, each asserted
  at the guarantee the knob breaks (the failure's text), not in the case's setup

## Contract

- **No backend.** Never `rusqlite`, `wsp-store-sqlite`, `wsp-store`, `wsp-wire`, `wsp-host` or `wsp-manager`
  (`tests/layering.rs`): a backend's tests depend on the suite, not the other way round.
- **Every case is a `pub fn` generic over `Harness`**, and returns `Err(CaseFailure)` when the backend breaks its
  guarantee (never a panic), so a backend's tests call them one by one and `unwrap()`. Threads and bounds in seconds; no sleep that
  hopes for an interleaving.
- **The cases bite**: each `Knobs` field breaks one guarantee, and a test shows its case then fails. A knob may
  fail other cases too (`gap_in_ids` hides the events cases 1, 2, 4, 9 and 10 read back), so a bite test asserts
  its own case only. Case 5 reads nothing back in its setup so that `gap_in_ids` fails it on order.
- **`crates/store-contract` is not edited here** (R5). What the suite needed and the contract lacks is wsp-065:
  the payloads of the events a store writes itself (leases, claims, `renew`), `set_text`'s principal, a
  command's fence, and idempotency's "first result" for a refusal. MemStore's answers are in `src/mem.rs`.
