# wsp-store-sqlite

The SQLite backend of the state store (`docs/state-core.md` sections 2.4 and 6). Step 0 only: the bundled
SQLite enters the tree, and nothing in wsp uses the crate yet. Schema, `apply`, leases and events come at
step 1, with the contract.

## Holds

- `src/lib.rs` — `sqlite_version()`, the version of the SQLite this crate bundles; the smoke tests: version
  >= 3.35, `RETURNING`, `json_extract`, and `journal_mode=WAL` sticking across connections

## Contract

- **`rusqlite` is a dependency of this crate and of no other** (D2; `tests/layering.rs`). It is pinned
  exactly (`=`) and `bundled`, so the SQLite version is ours and not the OS's. A bump is a row of its own.
- **No `rusqlite` type is exposed** (D1): the contract will name none, so another backend can replace this.
- **Links nothing of the workspace yet.** At step 1 it takes `wsp-store-contract` and `wsp-base` only, never
  `wsp-store`, `wsp-wire`, `wsp-host` or `wsp-manager` (state-core section 6; `tests/layering.rs`).
- Its C (`sqlite3.c`, in `libsqlite3-sys`) compiles through `.cargo/cc-wrap`, so sccache serves a second tree
  from cache (state-core section 9). `cargo build -vv -p wsp-store-sqlite` shows `CC=…/.cargo/cc-wrap`.
