# wsp-store-sqlite

The SQLite backend of the state store (`docs/state-core.md` sections 2.4 and 6), step 1: `SqliteStore`, a
whole `Store` on one WAL file that passes all ten conformance cases (`tests/conformance.rs`, one `#[test]`
each, no exceptions file). Nothing in wsp opens it until group 6.

## Holds

- `src/lib.rs` — `SqliteStore` (`open(path, domain)`, `open_read_only(path)`, `crash_next_apply`, the
  connection mutex, `tx()`), the `Error::Backend` mapping, `sqlite_version()` and step 0's smoke tests
  (version >= 3.35, `RETURNING`, `json_extract`, WAL sticking across connections)
- `src/schema.rs` — the DDL and the pragmas (WAL, 10 s `busy_timeout`)
- `src/apply.rs` — `Commands::apply` and what every write shares: the state row, stamping, folding
- `src/leases.rs` — `Leases` and `Claims`: the lease table, each change with its store-written event
- `src/reads.rs` — `Reads`, `Events` (`events_after`, `head`) and `Texts`; `wip_snapshot` is cut in SQL over the state row
- `src/events.rs` — `wait_for`: the process-wide wake registry and the `data_version` poll
- `tests/conformance.rs` — the `Harness` and the ten cases, plus the read-only and surface tests
- `tests/wip.rs` — `wip_snapshot`'s shape (bodies on open rows only, unfinished lists and finished lists a seat still sits on only, the other finished lists left out, raised asks only), and that it agrees with `MemStore`'s

## Schema

Tables `event`, `command` (idempotency key -> the `Applied` it returned), `lease` (`last` token kept across
releases, so tokens strictly increase), `state`, `text`. Every JSON column is `serde_json` of a contract type.
**`state` is one row (`id = 1`) holding the folded `State` as JSON**, not per-entity tables: `Domain::decide`
takes the whole `State` and `evolve` returns it, so one row is read and written whole, with no mapping that a
second backend would have to match; per-entity tables can follow when a query needs them.

## Inside `apply`

One `BEGIN IMMEDIATE ... COMMIT` (the lock between handles and processes), in this order: the idempotency key
(a seen key returns its first result), the fence, `Domain::decide` on the stored state, the events stamped
`MAX(id)+1..` and inserted, the state folded by `evolve` and written back, the key recorded. A refusal rolls
back and records no key. The store's own events (`lease.*`, `claim.*`, `text.set`) are written as
store-contract's table says, in the same transaction as the change.

## The wake

`wait_for` wakes in-process on a condvar: every handle on one canonical path shares a `Wake` (a generation
and a condvar) that a commit bumps after `COMMIT`. A commit by another process is seen by polling
`PRAGMA data_version` **every 50 ms** (`POLL`), so that is the bound across processes. The connection mutex is
held only for short reads, never while waiting.

## The crash hook

`crash_next_apply` arms a handle: its next `apply` decides, inserts half its events, then drops the
transaction uncommitted (as a killed process's would roll back), returns `Error::Backend`, and the handle is
dead. A reopen on the file holds all of the command or none (case 8, on a real file).

## Read-only open

`open_read_only` opens with `SQLITE_OPEN_READ_ONLY`, writes no schema and holds no domain. It reads
`snapshot()` and `events_after` while another handle writes (WAL); `apply`, leases, claims and `set_text`
return `Error::Backend`.

## Contract

- **`rusqlite` is a dependency of this crate and of no other** (D2; `tests/layering.rs`). It is pinned
  exactly (`=`) and `bundled`, so the SQLite version is ours and not the OS's. A bump is a row of its own.
- **No `rusqlite` type is exposed** (D1): no public signature names one, so another backend can replace this.
- **Links `wsp-store-contract` alone** (and `serde_json`); `wsp-store-conformance` is a dev-dependency only.
  Never `wsp-store`, `wsp-wire`, `wsp-host` or `wsp-manager` (state-core section 6; `tests/layering.rs`).
  **Only the root binary may link this crate.**
- Its C (`sqlite3.c`, in `libsqlite3-sys`) compiles through `.cargo/cc-wrap`, so sccache serves a second tree
  from cache (state-core section 9). `cargo build -vv -p wsp-store-sqlite` shows `CC=…/.cargo/cc-wrap`.
