# wsp as components

Plan `wsp-378`, 2026-11-05. Planning only: no code moved. The rows are on the worklist `components`, not started.
The decision is Ed's (see *The decision*).

Ed, 2026-11-05: *"so the architecture is far more modular, both for cheaper token costs and for faster builds and
tests."*

**Short answer.** The tree is not yet splittable into crates, and cutting it as it stands would have failed. 88 of its
103 modules (112k of 126k non-test lines) form one dependency cycle. Work that makes it acyclic comes first, and it
pays for itself. After that the cut is worth it for **tests and context** (a change reruns and loads about 40% of
the tests and a quarter to a third of the tree, against all of it today). For **build time** it is worth much less than
the premise says. A plain stack of crates rebuilds 78% of the code per change, because most changes land in the
middle of the stack. Only with contract crates at the seams does that fall to about 40–60%, and that has to be
measured at the first cut before anyone commits to the second. The numbers are below, with the scripts that
produced them in `docs/spikes/components/`.

## 1. What was found before, and what has moved since

`robustness-077` (2026-08-18, at review) started from "one crate compiles 113k lines to change one file" and found the
premise wrong in three ways. Three of its results bind this plan:

1. **The build cost was the release profile, not the structure.** `lto = false, codegen-units = 16` took a one-line
   release rebuild from 147s to 68s (decision d1 on that row, taken). A crate split touched neither number.
2. **The partition was mostly there.** 39 modules, counted without comments, formed *core → commands → surface*, with
   three edges the wrong way carrying eight symbols. `arrange` and `place` were closed seams; `draw` was not.
3. **Count dependencies with comments stripped.** The tree documents itself with `` [`crate::x`] `` intra-doc links,
   and counting them turned a 17-module knot into a 34-module one that did not exist.

What has changed in the ten weeks since: the crate went from 64.7k lines to **195.6k** (126.2k non-test and 69.5k
`#[cfg(test)]`, in 103 top-level modules; the packet said 189k and 2447 tests, `9abd4f3` has 2534). The clean layering
is gone. Over the same weeks the engine (`cycle`, `worklist`, `repair`, `verification`, `legs`, the barrier) grew
calls into the seat commands and back, and the `cmd_*` modules became libraries that everything else calls (`fold`,
`deliver`, `git`, `my_pane`). So the August answer, "the partition is mostly there", **no longer holds**, and the
cheapest part of the prior plan (second `[[bin]]`, `robustness-082`) is independent of it and still stands.

## 2. The dependency graph as the code has it

Method: `docs/spikes/components/graph.py` (run it from the repo root with `$WSP_SCRATCH` set, then `assign.py -v`, `estimate.py`, `tokens.py`, `outside.py`). Each `src/*.rs` and `src/{panel,detail}/` is one module. Comments and
strings are stripped. Everything from the final `#[cfg(test)] mod` on is counted apart, so a test that reaches up is
not mistaken for a product edge. Edges are `crate::m` paths and `use crate::{…}` lists, with the symbol named.

- **915 module→module edges in the product code.** One strongly connected component holds **88 modules, 112,284
  lines**; the only other cycle is `util ↔ place` (`SEAT_ENV`, one symbol). Leaves: `fm`, `choices`, `input`, `risk`,
  `warm`.
- **Hubs by fan-in:** `util` 84, `store` 76, `model` 57, `main` 46, `place` 40, `cycle` 31, `worklist` 30, `resolve` 27,
  `herdr` 24, `cmd_agent` 24, `cmd_checkout` 22. Fan-out: `cmd_agent` 48, `cycle` 37, `cmd_spawn` 35, `repair` 29.
- **`main` is a dependency of 46 modules, for one type.** `crate::Args` (and `build_stamp`, `stamp_word` in three).
  And `main.rs` is **edited by 36 of the last 81 commits that touched `src/`**, because it is the verb registry and the
  help text. That one file defeats any split of the files below it.
- **The low modules reach up.** `model` calls `agent_commands` (the `Kind` registry), `barrier` and `caption`. `store`
  calls `worklist::Running`, `cmd_govern::is_primary`, `place::new_agent_id`, `lapse`, `message`. `resolve` calls
  `place::is_agent_id`. Each of these is one to eight symbols, and none is deep.
- **Who writes a record.** `save_task` is called from **22 modules**, `save_worklist` from 10, `save_project` from 9,
  `save_message` from 3, `save_machine` from 1. 27 modules write files directly (`cycle` 14, `cmd_task` 5, `artefact`
  5, …). A Task is front matter plus sections in one `body` string, and several modules edit it by text.
  This is the same fault `compounding-004` measured on the selection cursor: seven deciders for one value.

### Assigning modules to components, and what is left over

`docs/spikes/components/assign.py` holds the assignment (the `put(...)` lines) and counts every edge that points
**up** the stack (a lower component using a higher one). That count is the work to do before any crate exists.

| try | what moved | upward edges | symbols |
|---|---|---|---|
| 1. nine groups by module name | nothing | 180 | 340 |
| 2. `cmd_checkout`, `cmd_verify`, `treecopy`… into a `trees` group; `Args`, `build_stamp`, git helpers, labels to base | 17 symbols, 13 modules | 120 | 268 |
| 3. `stage`, `caption`, `history`, `after` up to the engine; `daemon` and `fake` to a top group; 23 seat-identity and logging helpers to base/wire (`my_pane`, `local_backends`, `governs`, `is_primary`, `seat_for`, `log_line`, `is_agent_id`, `new_agent_id`, `alive`, `command`, `fold`, `read_source`, …) | 6 modules, 23 symbols | **80** | **207** |
| 4. tried: fold `check`, `greens`, `cold`, `affected`, `barrier_run` into `trees` | | 84 | 206 |

Try 4 did not help and is dropped: the knot is `cycle`, `worklist`, `verification`, `repair` and `barrier`, which
everything else hangs from. The 80 that remain, by direction (with `main` in `top` and `disk`, `memory` still in base):

| from → to | edges | what it is |
|---|---|---|
| engine → seats | **30** | `cycle` and `repair` spawn, end, rotate and wake seats; `cmd_task`, `cmd_message`, `cmd_worklist` ask who holds a seat. The real knot. `Seats` in `cycle.rs` is already a read-only port for this; it has to grow the act verbs |
| trees → engine | 13 | `cmd_checkout` and `check` use `worklist::Running`, `repair::LANDED`, `cycle::poke_task`, `cycle::launch_out` |
| store → engine / seats / wire | 7 / 3 / 2 | `kind_data` and `model` want `agent_commands::Kind`; `lapse` wants `worklist::crossed`; `store` wants `worklist::Running` |
| engine → timeline | 5 | `cmd_attempts::attempts_of` priced rows |
| seats → top, wire → engine, base → …, rest | 20 | `cmd_agent` asks `daemon::health`; `disk` and `memory` are one layer too low |

### The map the layering test reads

`tests/layering.rs` (row `p0`, wsp-400) reads this table: one row per component, lowest first, so a module may use
its own row and the rows above it in this table, never a row below. It is `assign.py`'s `put(...)` lines with `main`
in `top`, where §3 puts it (so today's `crate::Args` uses count as upward until `p1` moves `Args` to base), and the
modules added since the spike placed. A new module goes in this table or the test fails. `host` is the `wsp-host` crate's
modules (wsp-020), below the store because it uses none of it; the same file's
`the_host_crate_links_neither_the_store_nor_the_wire` holds the crate's own rule, which is about Cargo dependencies
rather than `crate::` paths. Every upward use the code
has today is listed in `docs/components-upward.txt`; the test fails on one not listed and on a listed one the code
no longer has, and that file may only lose lines. `cargo test --test layering -- --nocapture` prints the per-pair count.

Measured by it on 6 Nov (`ad10223`), with nothing moved yet: **171 upward edges, 335 symbols** (the spike gives the same 171
with this map; the 80 / 207 of try 3 assume `p1` and `p2` have moved their symbols), and **890 distinct
cross-component symbols over 60 of the 72 pairs** (the spike gives 890 too; the 499 / 49 below it does not reproduce).
Re-measured on 24 Nov (`9cf9a3e`, wsp-432) with the 7 Nov map: 82 upward edges, 145 symbols. With the
`engine` and `seats` rows re-derived (§5, phase 2) and the modules gone from the code dropped (`ended_panes`, `sync`,
`herdr`, `place_herdr`, `detect_override`): **76 upward edges, 133 symbols; 1229 distinct cross-component symbols over
44 of the 90 pairs**. Every module in the root package now sits in a phase 2 row or in `timeline`, `surface` or `top`,
except `base` and `testkit`, whose names are also modules of `wsp-base` and `wsp-store` and which stay with them.

| component | modules |
|---|---|
| `base` | `base`, `util`, `calls`, `fm`, `input`, `risk`, `choices` |
| `host` | `facts`, `hooks`, `inproc`, `jobs`, `port`, `price`, `shed`, `tcp`, `test_sups` |
| `store` | `model`, `checks`, `caption`, `store`, `resolve`, `message`, `lapse`, `lease`, `sharing`, `testkit`, `governors`, `agent_kind`, `verbs`, `decisions`, `log`, `tags`, `lands` |
| `wire` | `place`, `place_compound`, `place_super`, `host`, `host_tcp`, `host_inproc`, `tunnel`, `far`, `launchd`, `systemd`, `manager`, `fake` |
| `trees` | `cmd_checkout`, `cmd_verify`, `treecopy`, `check_tree`, `job_place`, `throttle`, `warm`, `overlap`, `guard`, `greens`, `at_stop`, `gc` |
| `engine` | `address`, `affected`, `after`, `artefact`, `attempt`, `attention`, `auto_close`, `barrier`, `barrier_replay`, `barrier_run`, `build_output`, `by_merge`, `check`, `classify`, `closing`, `cmd_allocate`, `cmd_hub`, `cmd_initiative`, `cmd_login`, `cmd_mandate`, `cmd_message`, `cmd_migrate`, `cmd_publish`, `cmd_submit`, `cmd_task`, `cmd_whoami`, `cmd_worklist`, `cold`, `cycle`, `deliver`, `did`, `disk`, `door`, `duplicate`, `ending`, `exit`, `fold`, `follow_up`, `history`, `hold`, `hold_close`, `hub_cache`, `hub_journal`, `hub_lease`, `hub_sync`, `inner`, `leg_seat`, `leg_tree`, `legs`, `list_sweep`, `liveness`, `local_trust`, `mirror`, `orders`, `outputs`, `owed`, `packet`, `packet_gen`, `pickup`, `piece_close`, `piece_seat`, `pieces`, `reconcile`, `record_workspace`, `refusals`, `repair`, `retro_yours`, `scope`, `scratch`, `size_check`, `split`, `stage`, `stand_down`, `starts`, `stepping`, `stuck`, `tell_log`, `untaken`, `verification`, `verifier_packet`, `waiting`, `waits`, `worklist` |
| `seats` | `agent_commands`, `cmd_agent`, `cmd_attend`, `cmd_brief`, `cmd_govern`, `cmd_install`, `cmd_machine`, `cmd_project`, `cmd_resume`, `cmd_role`, `cmd_sandbox`, `cmd_spawn`, `cmd_stamp`, `cmd_watch`, `kind_attention`, `kind_data`, `memory`, `room`, `wake` |
| `timeline` | `timeline`, `cmd_cost`, `cmd_burn`, `spend`, `cmd_calls`, `cmd_report`, `wallbuckets`, `cmd_attempts`, `cmd_verifications`, `retro`, `retro_run`, `retro_series`, `stale` |
| `surface` | `panel`, `detail`, `draw`, `kanban`, `live`, `story`, `arrange` |
| `top` | `main`, `registry`, `readonly`, `daemon`, `readings`, `cmd_piece` |

**499 distinct symbols cross a component boundary today, and 49 of the 72 possible component pairs have an edge.**
Those two figures are the seam count to ratchet down, in the sense of `compounding-004` (95 seams to 70).

## 3. The components

One cargo workspace, root package still `wsp` (the binary, `build.rs`, `main.rs` shrunk to a loop), members under
`crates/`. Dependencies point **down this list only**, and a dependency is on a contract (below), not on an
implementation, wherever the table says so. Lines are non-test, with `main` in `top`; tests are `#[test]` counts;
the test time is `cargo test <that component's modules>` on the 5 Nov test binary, **with the machine at load 16–30
on 8 cores** (so read it as an order of magnitude, and the shape matters more than any one figure).

| crate | holds | lines | tests | test s | writes |
|---|---|---|---|---|---|
| `wsp-base` | `util`, `fm`, `input`, `risk`, `choices`, `Args`, stamp, git helpers, the glyph table | 3.1k | 61 | 0.2 | nothing durable |
| `wsp-store` | `model`, `store`, `resolve`, `message`, `lapse`, `artefact`, `kind_data`, `sharing`, `scratch`, project/migrate/mandate verbs | 12.4k | 199 | 20.2 | **the records** |
| `wsp-wire` | `herdr`, `place` and its three backends, `tunnel`, `far`, `launchd`, `detect_override`, `memory` | 8.3k | 135 | 4.4 | the herdr/compound sockets |
| `wsp-trees` | `cmd_checkout`, `cmd_verify`, `treecopy`, `check_tree`, `build_output`, `throttle`, `warm`, `guard`, `leg_tree`, `overlap`, `disk` | 8.9k | 196 | 187.0 | worktrees, `.git`, build trees |
| `wsp-engine` | `worklist`, `cycle`, `repair`, `verification`, `check`, `barrier*`, `legs`, `closing`, `ending`, `hold*`, `cmd_task`, `cmd_worklist`, `cmd_message`, `stage`, `caption`, `history`, `after`, `packet*`, `attention` | 32.8k | 814 | 197.7 | the run's rows |
| `wsp-seats` | `cmd_spawn`, `cmd_agent`, `agent_commands`, `cmd_resume`, `cmd_govern`, `wake`, `room`, `cmd_machine`, `cmd_sandbox`, `cmd_watch`, `cmd_brief`, `cmd_install`, `cmd_stamp` | 30.1k | 634 | 33.4 | seat bindings, wakes |
| `wsp-timeline` | `timeline`, `cmd_cost`, `cmd_burn`, `cmd_report`, `wallbuckets`, `cmd_attempts`, `cmd_verifications` | 3.8k | 59 | 1.7 | nothing (reads) |
| `wsp-surface` | `panel`, `detail`, `draw`, `kanban`, `live`, `story`, `arrange` | 21.2k | 370 | 1.1 | nothing (reads, then asks) |
| `wsp` (bin) | `main`, `daemon`, `fake`; the verb loop | 5.4k | 66 | 0.1 | the process |

The two heavy test sets are **`trees` (196 tests, 187s) and `engine` (814 tests, 198s)**. 86% of test time is in two
components with 40% of the tests, and the cost in `trees` is `git init` per test, which is a fixture problem and
not a structure problem (row `c0-fixtures` below).

### Where the cuts stand

| crate | path | cut | holds now | map |
|---|---|---|---|---|
| `wsp` (root) | `src/` | — | every module not yet cut (since wsp-440 the `timeline`, `surface` and `top` rows); the binary, `build.rs`, the stamp (`src/base/stamp.rs`). Since wsp-075 it links `wsp-store-sqlite`, `wsp-server` and `wsp-store-contract` (the root is the only crate that links `wsp-store-sqlite`), and the daemon opens `$WSP_STATE/state.db` (with `wsp-server`'s composed domain under `Mode::Mirrored` since wsp-121), serves the API through `wsp-server`, runs the mirror thread (`src/mirror.rs`, files to contract entities, once a second), and `src/fold.rs` applies one `Command::Mirror` per manager event; `WSP_API=off` starts none of it and is the rollback | this file |
| `wsp-base` | `crates/base` | `c1` (wsp-408) | `util`, `fm`, `input`, `risk`, `choices`, `base::{args, git, labels, seat, term}` | `crates/base/MAP.md` |
| `wsp-host` | `crates/host` | M1 group 1 (wsp-020) | the in-process host: `InProcess` and the OS code it is built from (processes, signals, the screen, delivery, hook files, transcripts, host facts), and what it needed of the wire (`Seat`, `State`, `Refusal`, `Delivery`, the shed list, the prices); the wire re-exports each at its old path and keeps the `Host` trait as an adapter. Also the TCP carrier of `Connection` and the beat's host facts (wsp-021), and the hooks endpoint: the hook's post and the fold of hooks into `turn`, `seat.said`, `seat.spent` and `seat.did` (wsp-023). **Depends on `wsp-base` and `wsp-host-api`, never on `wsp-store` or `wsp-wire`**: `tests/layering.rs` walks its path dependencies | `crates/host/MAP.md` |
| `wsp-manager` | `crates/manager` | M1 group 2 (wsp-021) | the agent manager, binary `wsp-manager`: TCP on loopback and the tailnet, `hello` and its token, the journal and `subscribe {from}`, and the beat with the host's facts. Its seats' hooks post to it and it journals what they say (wsp-023). It runs seats (wsp-022) and tree jobs (`jobs`, wsp-050: `job.run` on `wsp-host`'s runner, capped per class, stopped by pgid, re-adopted at restart). **Depends on `wsp-host-api`, `wsp-base` and `wsp-host`, never on `wsp-store` or `wsp-wire`**: `tests/layering.rs` walks its path dependencies | `crates/manager/MAP.md` |
| `wsp-testkit` | `crates/testkit` | `c1` (wsp-408) | the git repository fixture (`repo`, `repo_at`, `git`); the store-backed half is in `crates/store/src/testkit.rs` (a `wsp-testkit` that depended on `wsp-store` would give the store's own unit tests a second `Store`) | its `lib.rs` |
| `wsp-store` | `crates/store` | `c5` (wsp-412) | `model`, `store`, `resolve`, `message`, `lapse`, `sharing`, `checks`, `caption`, `governors`, `agent_kind`, `log`, `decisions`, `tags`, `verbs`; the store-backed test kit | `crates/store/MAP.md` |
| `wsp-store-contract` | `crates/store-contract` | `c5` (wsp-412); state-core step 0 (wsp-046) | the constants the records name (`SCHEMA`, the section lists, `DEFAULT_LEASE`, the policy words); the file store's types stay in `wsp-store` while they carry inherent methods. Since wsp-046 also the state core's contract (docs/state-core.md 2.1, 2.2, 4.2, 4.3): the ids and `Principal`, the entities with section 3's states, `Event`, `Command` (the 19 verbs of 4.2, and since wsp-070 step 1's `mirror { put, gone }` with its `Entity` and the `mirror.put`/`mirror.gone` events, which only the server's mirror domain decides and steps 2–4 remove, and since wsp-117 step 2's orders machine: `order.send`'s `first`, `order.deliver` (fenced by `deliver:<seat>`, `LeaseName::deliver`) and the executor's answers `order.delivered`, `order.acked`, `order.refused` (`why`, `counts`) and `order.done`, with `first` and `why` on `Order`), `fields` maps on rows, seats, lists, groups and asks, `Fence`, `Refusal`, subscribe's `Feed`, the `Domain` (`decide`, `evolve`) that `apply` validates through, and the object-safe `Store` trait family. Types only (R2), no SQL or connection type (D1); nothing uses them yet. Depends on `serde` and `serde_json` alone | `crates/store-contract/MAP.md` |
| `wsp-store-conformance` | `crates/store-conformance` | state-core step 0 (wsp-047) | the conformance suite of docs/state-core.md 2.3: the `Harness` a backend implements (N handles on one store, a crash hook, reopen), the ten cases as `pub fn`s generic over it (`cases::case_atomicity` to `case_text`, one file each under `src/cases/`, each returning `Err(CaseFailure)` when its guarantee is broken), `MemStore` (the in-memory fake, public; its `Knobs` break one guarantee each, and `tests/mem.rs` shows the matching case 2, 4, 5 or 8 then fails) and `TestDomain`. All ten run against `MemStore` in `tests/mem.rs`. Nothing uses it yet; `wsp-store-sqlite`'s tests will at step 1. **Links only `wsp-store-contract` (and `wsp-testkit`)**, no backend: `tests/layering.rs` walks its path dependencies. Depends on `serde_json` beside them | `crates/store-conformance/MAP.md` |
| `wsp-store-sqlite` | `crates/store-sqlite` | state-core steps 0 and 1 (wsp-045, wsp-072) | the SQLite backend of the state store (docs/state-core.md section 6): `SqliteStore`, a `Store` on one WAL file that passes all ten conformance cases (`tests/conformance.rs`, one `#[test]` each, no exceptions file). One `state` row holds the folded `State` as JSON; each `apply` is one `BEGIN IMMEDIATE` (key, fence, decide, events, fold); `wait_for` wakes on a condvar in-process and polls `data_version` every 50 ms across processes; `open_read_only` reads and refuses `apply`. Since wsp-075 the daemon opens it (`$WSP_STATE/state.db`). **The one crate that depends on `rusqlite`** (D2), pinned exactly and `bundled`, its C compiled through `.cargo/cc-wrap`. Exposes no `rusqlite` type (D1). **Links `wsp-store-contract` alone** (`wsp-store-conformance` as a dev-dependency); no `wsp-store`, `wsp-wire`, `wsp-host` or `wsp-manager`, and only the root may link it: `tests/layering.rs` walks its path dependencies | `crates/store-sqlite/MAP.md` |
| `wsp-api` | `crates/api` | state-core step 1 (wsp-071) | the state core's API on the wire (docs/state-core.md section 4): `Frame` (tagged `type`), `hello`/`welcome`/`refused` with the instance's token (`api.token`, 0600) and `api.json`, where the server listens (TCP on loopback and the tailnet, D19, in place of 4.1's `api.sock`), 4.1's `Request` envelope and `Reply`, the read verbs of 4.2 and `subscribe`, the contract's `Command`s named so a step-1 server answers them `unsupported`, `Answer`s and error `Code`s, `Feed` frames, `Principal`'s text form (`person:ed`), and its own `Connection` trait with host-api's framing rule copied (the in-memory pair behind `test-support`). Since wsp-459 also the hub's sync verbs (`Call::Sync`: `sync.heads`, `sync.have`, `sync.put`, `sync.get`, `refs.move`, a stale move answered `ok` with the hub's head). Types only (R2), no IO; `wsp-server` (wsp-073) uses it; `wsp-client` (wsp-074) will. **Links only `wsp-store-contract` and, since wsp-459, `wsp-objects`** (pure, no IO; it brings `sha2`), beside `serde` and `serde_json`: `tests/layering.rs` walks its path dependencies and its source | `crates/api/MAP.md` |
| `wsp-server` | `crates/server` | state-core step 1 (wsp-073) | the state core's server (docs/state-core.md section 6), step 1's part: `Server::start` on loopback and the tailnet (D19), refusing any peer outside `127.0.0.0/8`, `::1`, `100.64.0.0/10` and `fd7a:115c:a1e0::/48`, `api.token` made at first start (0600), `api.json` written with what it bound, refusing to start while anything answers its port; per connection `hello` first, the read verbs of 4.2 as one store call each, the view verbs `throttle`, `spend` and `show` answered by the `Views` the daemon hands in (wsp-307), every command `unsupported` but the orders machine's (wsp-121: `order.send` and `order.cancel` from a person or a seat, `order.deliver` and the executor's answers from `wsp:<instance>` or a manager and `unauthorised` from anyone else, each with a key, applied through the store and answered `applied` with its events), a malformed line answered `malformed` with the connection kept; `subscribe {}` / `{from}` (snapshot or replay, then the head through `wait_for`, a beat every `BEAT_SECS`, a client silent for `MISSED_BEATS` dropped); `tcp::Tcp`, the carrier (public for wsp-client's tests); `mirror::Mirror`, the domain step 1's store runs (`mirror.put` for what changed, `machine.beat` for a machine whose beat and liveness facts alone moved (wsp-304), `mirror.gone` for what is present, nothing for what is unchanged); `domain::Composed`, the mirror and `wsp-machines`' orders machine as one domain under a `Mode` (wsp-121): `Mirrored` (the mirror puts orders, the machine's verbs `illegal`, state `mirrored`), `Owned` (the machine decides them, a `mirror` naming an order `illegal`, state `owned`), its fold the same under either. Over any `dyn Store`; tested over `MemStore` (`tests/server.rs`). Since wsp-075 the daemon starts it (`WSP_API=off` does not). Answers the sync verbs `unsupported` unless started with the `SyncVerbs` the hub hands in (wsp-459, wsp-460), and admits the peer ranges `Config::peers` names (`ranges::Ranges`, CIDR blocks) in place of loopback and the tailnet when it is given them. Since wsp-461 `Config::tls` (`tls::server_config`, rustls with `ring`) puts every accepted connection inside TLS, which only the hub sets; the peer rule is the same with or without it. **External dependencies: `serde_json`, `rustls`** (and `rcgen`, optional, behind `test-support`). **Links `wsp-api`, `wsp-store-contract` and `wsp-machines` alone**, and `wsp-objects` (pure, `sha2`) through `wsp-api` since wsp-459, never as a dependency of its own (`wsp-store-conformance` as a dev-dependency), never the sqlite crate: `tests/layering.rs` walks its path dependencies | `crates/server/MAP.md` |
| `wsp-client` | `crates/client` | state-core step 1 (wsp-074) | the state core's client (docs/state-core.md section 6), step 1's part, the one way a process outside the daemon talks to the API (the CLI, wsp-076, and web-chrome, wsp-078, by path): `Client::dial(state_dir, by, wait)` reads `api.json` and `api.token`, dials the addresses in turn within `wait`, says `hello`, and is `Silent` (no file, no token, nothing answering, no welcome, a refused token) with a reason that names which, never a panic or a hang; `Client::request(verb, args)` is one reply matched by id with one timeout, and the next request after a lost connection dials again; `Client::subscribe(from)` is a `Feed` that hands out `Item::State`, then each event once (an id already handed out is dropped, 4.3), counts missed beats, and on a closed connection or `MISSED_BEATS` quiet beats dials again and subscribes from its last id, announced as one `Item::Resumed { from }`; `too_old` leads to `subscribe {}` and a fresh `State`. Its TCP carrier is a dial-side twin of the server's, so a caller does not link the server. Since wsp-461 a hub URL of `tls://host:port` is dialled over TLS (the platform's roots through `rustls-native-certs`, or one pinned CA file: `Client::dial_hub_ca`, `--ca`, `$WSP_HUB_CA`, `ca` in `hubs.json`), a refused certificate is `Silent::Tls`; `tcp://` and a bare `host:port` stay plain. Since wsp-459 `Client::sync(workspace, &Refs, &impl ObjectSource) -> SyncReport` syncs a workspace with a hub by hash (a stale move reported, never retried), and `testing` (behind `test-support`) holds `MemHub`/`FakeHub`, the in-memory reference hub until wsp-460. Nothing calls it until group 7. **Links `wsp-api`, `wsp-store-contract` and, since wsp-459, `wsp-objects` alone** (pure, no IO; it brings `sha2`), beside `serde_json` and, since wsp-461, `rustls` and `rustls-native-certs`, so web-chrome, which links the client by path, now links `wsp-objects` and `sha2` through it; `wsp-server` and `wsp-store-conformance` are dev-dependencies (a loopback server over `MemStore`, `tests/client.rs`): `tests/layering.rs` walks its path dependencies | `crates/client/MAP.md` |
| `wsp-hub` | `crates/hub` | multiplayer v1 (wsp-460) | the hub, a separate minimal service (`wsp-hub serve --config <file>`; the file is JSON: `data_dir`, `bind`, `port`, `peers`, `name`, loopback by default; `tls` {`cert`, `key`} (wsp-461) serves every connection over TLS and is required to bind any address that is not loopback): objects in `objects/ab/cdef…` keyed by hash (a temp file, a rename, an id present is not rewritten), refs and leases in one `hub.db` through `wsp-store-sqlite` (`refs.move` a compare-and-swap in one transaction, a lease's tokens the sqlite crate's own), and the verbs of wsp-459 answered through `Hub::answer`. Served on `wsp-server`'s carrier and connection loop (hello with the admin token `api.token` in the data dir, requests, subscribe), handed `Hub` as its `SyncVerbs` and `HubStore` as its store, whose event log reads the hub's announcement row as `ref.moved { workspace, subject, to }` (the subject's head now). Runs no agents and builds nothing. **Links `wsp-api`, `wsp-objects`, `wsp-server` (and `wsp-machines` through it), `wsp-store-contract` and `wsp-store-sqlite` only** (the second manifest, after the root's, that may name the sqlite crate); never the root, `wsp-store`, `wsp-manager`, `wsp-trees` or `wsp-host`: `tests/store.rs`'s `cargo tree` test and `tests/layering.rs`. `wsp-hub admin init --config <file> --admin <name>` (wsp-163, `admin`) makes the first admin through the bootstrap principal and prints its token once (the hub keeps the hash); the other admin verbs are the root crate's `wsp hub user|token|workspace|member` (`cmd_hub`), from a logged-in admin's spoke. `tests/serve.rs` runs the binary: two clients converge through it across a kill and a restart | — |
| `wsp-engine` | `crates/engine` | phase 2 (wsp-440) | the `engine` and `seats` rows of the map below, cut whole (§5, phase 2): 97 modules, the run (`worklist`, `cycle`, `repair`, `verification`, `legs`, the barrier), the row and list verbs, orders, the fold and the mirror, and the seat commands (`cmd_spawn`, `cmd_agent`, `agent_commands`, `wake`, `room`, `cmd_watch`, …), with `disk` and `memory`. The root re-exports each at its old path (`main.rs`); every `pub(crate)` in them is `pub`. The build stamp is the root's (`build.rs`), so `main` hands it in (`base::stamp::BUILD_STAMP`, as `store::BUILD_STAMP`); timeline's report writers likewise (`outputs::install`), with stand-ins in the crate's own tests. Doc blocks are not doctests (`doctest = false`). **Normal dependencies: `serde_json` and the members below it** (`wsp-base`, `wsp-store`, `wsp-wire`, `wsp-trees`, `wsp-host-api`, `wsp-store-contract`, `wsp-objects`, `wsp-server`, `wsp-api`, `wsp-client`, `wsp-view`, `wsp-machines`); `wsp-store-sqlite`, `wsp-hub`, `wsp-manager`, `wsp-host` and `wsp-testkit` are dev-dependencies, the sqlite crate for tests alone: `tests/layering.rs` walks its path dependencies | `crates/engine/MAP.md` |
| `wsp-machines` | `crates/machines` | state-core step 2 (wsp-118) | the state core's machines (docs/state-core.md sections 3 and 6), each a pure `Domain` over the contract; step 2's part is `orders::Orders`, 3.1's machine: `Orders::owns` (the seven `order.*` verbs, for the server's routing), `decide` (`order.send` → `queued`; `order.deliver` → `typed` by the holder of `deliver:<seat>` at its current token, to a seat `idle` or `working` (`started` for a `first` order) with no other order `typed`; the executor's `delivered`, `acked`, `refused` (`attempts + 1`, `failed` at `ATTEMPTS` = 3; `counts: false` no attempt) and `done`; `cancel` from any state but `done`), every refusal one of the contract's codes and drafting nothing, a repeat answer an empty decision; and `evolve` (the `order.*` events and `lease.*`). One test per transition and a table of every illegal one in `tests/orders.rs`, which stands in for conformance case 9 (fixed to `TestDomain`). `wsp-server`'s `domain::Composed` composes it beside the mirror (wsp-121). **Links `wsp-store-contract` alone**, beside `serde_json`, and its source names no IO and reads no clock: `tests/layering.rs` walks its path dependencies and its source | `crates/machines/MAP.md` |

`disk` and `memory` moved with the phase 2 crate (wsp-440); since wsp-432 the layering table maps them where they
live, `disk` to `engine` and `memory` to `seats`, and neither reaches above its row but `disk -> cmd_sandbox` (inside
the phase 2 crate). Dependencies are `serde_json` only (and `rusqlite`, in `wsp-store-sqlite` alone: D2); the members are path dependencies in the workspace.
The test count is `cargo test --workspace -- --list` = 2756 at `c1` (2754 `#[test]`, the same before and after, plus
the two layering tests); the 2534 above is the figure of 5 Nov.

### The contract of each, and who writes what

The rules are `docs/widget-contract.md` in compounding, applied rather than copied:

- **R1, one writer per relation.** Every durable fact has one owning crate. Any other crate that wants it changed
  calls the owner's verb; none edits the record. A section of a record (`## Verification`, `## Log`, `## Decisions`)
  is a relation.
  Section owners today: `## Verification` → `verification.rs` (`save_runs`, `carry_pass`, `write`; `write_runs` is private to it, and `check.rs`/`cycle.rs` call the verbs); `## Checks` → `check.rs`; `## Legs` → `legs.rs`; claims (`claims.json`) → `store.rs` (`set_claim`, `clear_claim`, `reap_claims`; no other module opens the file, and callers go through the verbs). `## Log` → `log.rs` (`note`, `note_to_list`, `append`, `append_to_list`, `append_to_body`; `Task::log`/`Worklist::log` delegate to it). `## Decisions` → `decisions.rs` (`decide_task`, `decide_project`; callers go through the verbs). tags → `tags.rs` (`adjust`; `wsp tag` calls it).
- **R2, a contract is a crate with no logic.** Types, traits and constants only, for the symbols that cross. Others
  depend on `<crate>-contract`, never on `<crate>`; only the binary depends on implementations. This is what keeps
  an edit in an implementation from rebuilding its dependents.
- **R3, a component runs without the host.** Its tests build their own store in a scratch directory and take
  every other component as a trait object. `Seats` in `cycle.rs` and `Source` in `attention.rs` are the model already
  in the tree. `scratch()` is written 29 times today (one per test module): it becomes one `wsp-testkit` dev-crate.
- **R4, small enough for one agent to hold.** A crate is under ~35k lines, and the map is under 60 lines. `engine`
  and `seats` are over that and are the second-phase cut (section 5).
- **R5, a contract changes only by a row of its own**, and the layering test (row `p0`) fails a build that adds a
  crate-to-crate edge the contract does not name.
  It applies to every contract crate: `wsp-store-contract`, `wsp-host-api` and, since wsp-071, `wsp-api`
  (docs/state-core.md section 6).

| crate | its contract (what others may use) | one-writer facts (only this crate…) |
|---|---|---|
| `wsp-base` | `Args`, `Stamp`, `git(…)`, `Clock`, `Colour` | …owns no state. Anything stateful is refused here |
| `wsp-host` | `InProcess` and its methods, the spawn and stop types, `Seat`, `State`, `Refusal`, `Delivery` | …runs every OS operation a seat needs: `ps`, `kill`, `lsof`, the fact probes, `compound-render`, transcript and hook-log reads (`tests/host_seam.rs` allows them under `crates/host/src/` alone). Owns no record; links nothing of the store, so the agent manager can link it |
| `wsp-store` | `Store` (open/read/save/lock), `Task`, `Project`, `Worklist`, `Message`, `Machine`, `Status`, the section accessors | …writes the task, project, worklist, message and machine records and `~/.local/state/wsp`; **one verb per section** (`log`, `decide`, `verification`, `claim`), so the 22 `save_task` callers become calls. `lapse` runs inside the store's own writes, as it does today |
| `wsp-wire` | the `Place` port and its eight verbs, `Seat`, `State`, `Refusal`, `Backend` list | …talks to herdr, compound and supervisors. Nothing else opens a socket or an ssh. The fake backend lives here |
| `wsp-trees` | `Tree`, `Flow`, `land`, `verify`, `TreeLock`, `Throttle` | …creates, rebases, removes and locks worktrees and build trees; runs the throttled cargo |
| `wsp-engine` | `Run` (step, tick), the `Fleet` port (below), `Step`, `Verdict`, `Barrier`, `Leg` | …moves a member through its states; **the only writer of `## Verification` and the barrier entries**; the only caller of `Fleet` |
| `wsp-seats` | `Fleet` impl (spawn, end, rotate, wake, say), `Kind`, `Address`, `Ran`, `Spend`, the seat binding | …binds a seat to a row, and starts and ends agents; the only writer of seat bindings and wakes |
| `wsp-timeline` | `Attempt`, `Cost`, `Report`, `Timeline` | …none; derives from the log and transcripts |
| `wsp-surface` | `Frame`, `Panel`, `Target` (draw's two-method trait) | …none; every action is an `Ask` to the owner's verb, as compound's widgets do |

The one new interface is `Fleet`: the act side of `Seats`. It is the 30 `engine → seats` edges rewritten as a trait of about 12
methods (`spawn`, `end_when_idle`, `rotate`, `wait_for_pane`, `say`, `held`, `settle`, `governor_of`, `occupant`,
`reseat`, `mark_stood_down`, `counted`) which `wsp-seats` implements and a test fake implements. That is 30 calls
becoming one 12-method contract, the same shape as compounding's 95→70.

Tests and fixtures stay beside the code: `crates/<c>/src/…` carries its `#[cfg(test)]` modules; the `fixtures/`
directory (44K, two jsonl files) goes to the crate that reads them (`wsp-seats`); the store-backed test kit and the
git fixture become `wsp-testkit`.

## 4. What each saves

**Method and limits.** All timings 5 Nov, `9abd4f3` + a probe function added to one file and reverted, a private
target dir, real `cargo` (the throttle shim queued the first run for 216s, so it was not used), the machine at load
6–30 on 8 cores. They are single runs, accurate to perhaps ±30%. The "after" figures are estimates from the
measured ones, and say how.

### Build

| measured now | |
|---|---|
| `cargo test --no-run` after a one-function edit | util 21s · cmd_spawn 24s · panel/rows 24s · store 32s (a no-op was 15s) |
| `cargo build --release` cold | 252s (August: 91s at 113k lines, load 4) |
| `cargo build --release` after one edit in `cmd_spawn` | **198s, 79% of cold** (August: 74%) |

The edit cost does not depend on which file is edited: 21 to 32s in all four cases, and the crate is one front end.
The release edit is nearly a cold build, as `robustness-077` found, because release builds are not incremental.

The estimate uses the last 81 commits that touched `src/` (since 22 Oct), the files each touched, and the crate graph
above (`estimate.py`). **The rebuild set of an edit is the crate plus every crate that depends on it.**

Share of the 126k non-test lines rebuilt by an edit in: base 100%, store 96%, wire 86%, trees 79%, engine 72%,
seats 46%, timeline 20%, surface 17%, top 2%. Commits touching each (81 commits; one commit can touch several):
engine 55, base 39*, seats 34, trees 28, store 14, timeline 10, wire 6, surface 2.

*39 includes 36 commits that touch `main.rs`, which becomes the verb loop and falls out of this once verbs register
from their own crates (row `p3`).

- **As a plain stack, a commit rebuilds 78% of the code on average** (median 81%). Splitting alone does not buy the
  build time. Most changes land in `engine`, `seats` and `store`, which sit low and have many dependents.
- **With contract crates (R2) a commit that stays inside an implementation rebuilds that crate plus the binary, 40% of
  the lines on average.** A commit that changes a contract still rebuilds everything above it. At 20% of commits
  changing a contract the mean is 52%; at 35%, 61%. What share changes a contract is unknown today, and it is the
  number the first cut measures.
- Rebuild time is not linear in lines (the front end and link are fixed costs), so read the shares as an upper
  proxy. The test is concrete: after cut `c1`, edit `wsp-surface`, and `cargo test --no-run` must come in under half the
  21–32s above. If it does not, stop at `c2`.

### Tests

| measured now | `cargo test` whole: **409s**, 2526 passed, 1 failed, 7 ignored (`launchd::…second_install_leaves_it_alone`; not looked into: it touches launchd, which nothing here changed) |
|---|---|
| per component | the *test s* column in section 3: sum 446s (the filters overlap a little) |

Tests are where a split pays, and the machinery is already in the tree: `wsp-361` (`affected.rs`) maps a changed path
to its crate and then to its reverse dependents, and the greens ledger keys a pass by the crate directories and their
workspace dependencies, so a green is reused at any commit where those are unchanged. Neither does anything on one
crate and both start working on several.

- **Estimated after:** a commit reruns the tests of its crates plus their dependents' tests only if a contract
  changed. Own components only is **39% of tests on average** (the stack figure with dependents is 79%). With the
  heavy git tests fixed (`c0-fixtures`) `trees` falls from 187s to what the 196 tests need without a `git init` each,
  and I have not measured that.
- A verifier's `cargo test <module>::` becomes `cargo test -p wsp-seats <module>::`; `packet_gen` and `affected`
  emit it (row `c1`).

### Context an agent loads

From this week's transcripts: 224 files under `~/.claude/projects/*wsp--worktrees*` modified in the last seven days;
112 seat sessions with a row title, 4,524 requests deduped by `requestId`, 448.6M cache-read, 11.0M cache-write, 2.9M
output tokens. Tool results counted at 3.6 bytes a token (`tokens.py`, `outside.py`).

- **Agents find code with `sed`, `grep` and `cat` in Bash, not `Read`.** 1,691 Bash results that read `src/` hold
  2.05M tokens; 22 `Read`s of `src/` hold 0.09M. Re-read on every later request, that is **95M of 448M cache-read tokens (21%)**.
  The rest is the brief, tool output that is not code, and the agent's own turns, and no structure touches it.
- **Per seat session, code-finding is a median 11.7k tokens (p90 44k, mean 19k).** The seven files over 5k lines are
  27% of the file-attributed tokens (`cycle` 250k, `repair` 156k, `check` 132k, `cmd_spawn` 113k, `main` 90k).
- **36% of the named-file tokens were outside the component the session edited** (51 sessions that edited `src/`; the
  median session 31%). 29 of 51 edited one component, 16 two, 6 three or four. That is the most a crate boundary and
  a per-crate map can remove, and it leaves out the grep hits in files never named.
- **Estimated after: 36% of 21% is about 7% of cache-read tokens**, by scoping alone. The larger lever is shorter
  sessions from faster edit loops, which this plan does not claim. It is not the headline, and the cost argument for
  this work rests on build and test time, not on tokens.
- The map today is `README.md` `## Source map`: 85 lines, 13.9 KB (about 3.9k tokens), in a README of 5,749 lines (356 KB).
  A per-crate map of 60 lines (about 0.6k tokens) plus this file's table is what an arriving agent loads, so
  about 3k tokens saved at the start of each session, small against the above.

## 5. Migration order

Strangler, one cut per pass, the live `wsp` working at every step. The binary stays `wsp` built from the root package,
so `wsp install`, `build.rs` (the commit stamp), the throttle shim and `warm.rs` (copies `target/` to a new tree) are unchanged.
**Every row follows the rules of this repo:** one commit series per row in its own tree; rebase onto master before `wsp
verify --release`; `wsp review`; a verifier lands it. A cut row is **pure moves**: `git mv` (so a concurrent edit to a
moved file rebases across the rename), `Cargo.toml`, `pub use` shims at the old paths so no caller changes, and no
logic edit. The check on every cut: `cargo test -- --list` counts the same tests before and after (4102 today).
Cut rows are serial: a second cut does not start until the first has landed and been installed.

**Phase 0, seams inside the one crate (no crate yet; each row stands alone and removes build and context cost on its own):**

| row | what | edges / symbols removed |
|---|---|---|
| `p0` layering test | A `#[test]` that parses `src/` (as `graph.py` does), reads the component map from `docs/components.md`, and fails on any new upward edge; prints the per-pair symbol count; the ratchet | 0, but it makes every later row measurable |
| `c0-fixtures` | One `wsp-testkit` module: the scratch store and the git repo fixture (a template repo copied, not `git init` per test) replacing 29 `scratch()` and ~20 git setups | none; targets the 187s and 198s |
| `p1` base | `Args`, `build_stamp`, `stamp_word`, git helpers, `WORKTREES`, glyphs, `stty`, `SEAT_ENV` into `base/` | 46 modules' `crate::Args`; ~17 symbols; ends `util ↔ place` |
| `p2` identity hubs | `my_pane`, `local_backends`, `governs`, `is_primary`, `seat_for`, `log_line`, `alive`, `new_agent_id`, `is_agent_id`, `fold`, `read_source`, `Kind`, `Address`, `Ran`, `Spawn` to the module that owns the idea | the 120 → 80 step above |
| `p3` verb registry | Each `cmd_*` exports `VERBS: &[Verb]` (name, usage lines, `run`); `main` loops; help text moves with the verb | `main.rs` edits fall from 44% of commits to the registry only |
| `p4` writer sections | One store verb per record section; 22 `save_task` callers become calls; sections stop being edited by text | the R1 fact; adds nothing to the edge count |
| `p5` the `Fleet` port | The act side of `Seats`: ~12 methods in `cycle.rs`, `cycle`/`repair`/`legs`/`stand_down` call it, `cmd_spawn` etc. implement it | the 30 `engine → seats` edges |
| `p6` trees ↔ engine | `Running`, `LANDED`, `poke_task`, `launch_out` become parameters or move down | the 13 `trees → engine` edges |

The layering test's count must read **0 upward edges** before the first crate. Order within phase 0 is by edges
removed per risk; `p0` and `c0-fixtures` are first because they are independent and give the measure.

**Phase 1, cut by fewest remaining seam edges, bottom to top where the graph requires it:**

| row | crate | gate before the next cut |
|---|---|---|
| `c1` | workspace skeleton + `wsp-base` (+ `wsp-testkit`). `wsp verify`, declared `## Checks`, `affected`, `packet_gen` taught `-p` | install the result; edit `wsp-base` once and time it; the 4102 count |
| `c2` | `wsp-store`, after its seams (wsp-460) | edit it once and time it; the count of tests |
| `c3` | `wsp-wire`, after its seams | |
| `c4` | `wsp-trees`, after its seams | |

After phase 2 (`engine` and `seats` cut): `wsp-surface` and `wsp-timeline`. The **surface build gate** (formerly `c2`) moves there: edit `wsp-surface`, time `cargo test --no-run`, **under half the 21–32s, else stop**. It is measurable only once `engine` and `seats` are cut, because until then nothing below `surface` is a crate to rebuild around.

**Phase 2, gated:** `engine` and `seats` (63k lines, 50% of the code) are cut only after `p5` has held for a
fortnight and the contract-change share is known. If the share is above ~35% the gain is under 40% and the cut is not
worth the churn, and the plan stops at six crates.

**Phase 2's sets, re-derived on 24 Nov (wsp-432, `9cf9a3e`).** The map rows above, from today's `src/` and
`cargo test --test layering -- --nocapture`; lines are non-test (up to the first `#[cfg(test)] mod`), counted by the
test's own split. The root package holds 131.3k such lines in 125 modules.

| set | modules | lines | upward lines out (module pairs) | distinct symbols, per pair as the test prints |
|---|---|---|---|---|
| `engine` | 78: the 7 Nov row less `ended_panes`, `cmd_attend` and `kind_attention`, plus `stuck`, and the root's `artefact`, `cmd_hub`, `cmd_mandate`, `cmd_migrate` (mapped to `wire` and `store`, never cut) | 66.2k | **49**: 28 to `seats`, 5 to `timeline`, 1 to `surface`, 15 to `top` | 59 / 12 / 1 / 2 |
| `seats` | 19: the 7 Nov row less `ended_panes`, `sync` and `stuck`, plus `cmd_attend`, `kind_attention` and `memory` | 34.7k | **17**: 2 to `surface`, 1 to `timeline`, 14 to `top` | 6 / 1 / 8 |
| one crate, `engine` + `seats` | 97 | 100.9k (77% of the root) | **38**: 29 to `top` (26 of them `-> main: Args`/`build_stamp`/`stamp_word`, 3 `-> daemon`), 6 to `timeline`, 3 to `surface` | 8 / 12 / 6 |

The moves that cut edges, each found by trying every single and every pair of `engine` ↔ `seats` moves against
`docs/components-upward.txt` (a move may only delete lines from it, since the test bounds the list by the trunk's):
`cmd_attend` and `kind_attention` to `seats` (−3 lines: their uses of `agent_commands` and `cmd_agent`), `stuck` to
`engine` (−1: `repair -> stuck`), `memory` from `base` to `seats` (−2: `memory -> cmd_spawn`, `memory -> place`).
No other single or pair move cuts a line without adding one: `wake`, `room`, `cmd_govern`, `agent_commands` and
`attention` each add more than they remove, because `seats` uses `engine` through 219 symbols and `engine` uses
`seats` through 59. The two rows are one tangle, and the map can be moved no further without moving code.

**Phase 2 is one crate, `engine` and `seats` together.** Two crates cost the 28 `engine -> seats` lines (59 symbols:
`cycle`, `repair`, `stand_down`, `stage`, `waits` into `cmd_govern`, `wake`, `room`, `cmd_agent`, `cmd_spawn`,
`agent_commands`, 15 lines; `cmd_task`, `cmd_worklist`, `cmd_message`, `leg_seat` into the same, 10; `attention ->
cmd_watch`, `attention -> wake`, `disk -> cmd_sandbox`, 3), which is the `Fleet` port `p5` was meant to finish and did
not, plus a second serial cut. What two buys is a seats-only edit rebuilding 34.7k lines instead of 100.9k; an edit
to `engine` rebuilds `seats` either way. Of the 689 commits since 25 Oct that touch `src/`, 80 (12%) touch `seats`
modules alone, against 195 `engine` alone, 95 both and 201 either beside other sets. So the crate is cut whole:
the 38 lines out of it are the gate (`-> main`, `-> daemon`, `-> panel`, `-> cmd_attempts`, `-> cmd_cost`,
`-> cmd_report`), the 28 `engine -> seats` lines stay in the list as a ratchet inside the crate, and the map keeps two
rows so a later split starts from them. The seam rows that are left: wsp-434 (`-> main`, 26 lines out of the crate, 10
more from `timeline` and `surface`), wsp-435 (`-> daemon`, `-> panel`, 6 lines), wsp-436 (`-> cmd_attempts`,
`-> cmd_cost`, `-> cmd_report`, 6 lines), then the one cut, wsp-440. wsp-437, wsp-438, wsp-439 and wsp-441 leave the
path to the cut.

Hazards the order is built around: the cut rows touch every file in flight (rename detection is the only protection,
hence pure moves and `git mv`); `tests/`-style paths in checks (`wsp check` scripts in each project's `## Checks`,
and the `affected` key) must name the new paths in the same row; and **`serde_json` stays the only external
dependency**, with one amendment: **D2 (docs/state-core.md section 6) admits `rusqlite`** as the second, in
`wsp-store-sqlite` only, pinned exactly and `bundled`; `tests/layering.rs` fails when any other manifest names it. A
workspace's members are not dependencies in the sense of that rule, and the commit should say so.
A single binary linked from the members costs nothing at start-up (`robustness-077`).

## 6. The maps, and how the brief points at them

**Per crate: `crates/<name>/MAP.md`, under 60 lines**, written in the cut row and kept in the same commit as any
change that moves a boundary. Fixed shape, so an agent reads one the same way every time:

1. *Holds* — one line per module, what it owns.
2. *Contract* — the exported symbols, from `<name>-contract`, with the one-writer fact for each record.
3. *Does not hold* — three or four things people look for here and which live elsewhere, with the crate.
4. *Tests* — the command (`cargo test -p wsp-seats <module>::`), the fixtures, and what is slow.
5. *Files that change most* — from `git log`, so a change starts in the right file.

`docs/components.md` (this file, cut to its tables when the work is done) is the one map above them: crate, one line,
lines, and the contract list. `README.md`'s `## Source map` is deleted at `c1` and replaced by a link, so there is no
second copy to go stale.

The brief points at it in three places, all in the cut row, and none adds output (`wsp brief` is paid for on every
request, so each costs nothing new):

- **The handbook line** about the map says: read `docs/components.md`, then the `MAP.md` of the crate the packet's
  *likely files* name, never the README.
- **`wsp packet`**'s `likely files` line carries the crate beside each path (`crates/seats/src/cmd_spawn.rs`), and
  the `verify` line carries `-p <crate>`; `packet.rs` already derives both from paths.
- **A task that crosses a boundary says so**: the packet prints the contract it changes, which is how an agent knows it is
  editing a seam, not a part.

## 7. The rows

On the worklist **`components`**, status `draft`, nothing started (`wsp worklist show components`). Groups 1–4 are
phase 0 and groups 5–10 the cuts, one crate to a group. The names used above are the keys:

| key | row | group | | key | row | group |
|---|---|---|---|---|---|---|
| `p0` layering test | wsp-400 | 1 | | `p6` trees ↛ engine | wsp-407 | 4 |
| `c0-fixtures` test kit | wsp-401 | 1 | | `c1` workspace, base, testkit | wsp-408 | 5 |
| `p1` base | wsp-402 | 2 | | `c2` surface | wsp-409 | 6 |
| `p2` identity hubs | wsp-403 | 2 | | `c3` timeline | wsp-410 | 7 |
| `p3` verb registry | wsp-404 | 3 | | `c4` wire | wsp-411 | 8 |
| `p4` writer sections | wsp-405 | 3 | | `c5` store + contract | wsp-412 | 9 |
| `p5` Fleet port | wsp-406 | 4 | | `c6` trees | wsp-413 | 10 |

Phase 2 is not filed: it is a decision after `c2`'s gate and `p5`. `wsp-399` is a duplicate of `p0` filed by mistake
and left off the list; it should be retired.

## 8. The decision

Three ways to go, cheapest first, and the choice is Ed's:

- **A. Phase 0 only.** The seams inside one crate: acyclic, layering test, `Fleet` port, verb registry, fixtures. Cost:
  eight rows, no crates, no change to how `wsp` is built or installed. Buys: the test fixture time, `main.rs` stops
  being a merge point, the knot is measurable, and every later cut becomes a move. This is worth doing even if
  nothing after it is.
- **B. A, then the outer crates** (`base`, `surface`, `timeline`, `wire`, `store`, `trees`: 58k lines, 46% of the code
  out of the middle). Buys the context and test figures above, and a build gain only where the gate says so.
  **Recommended.** It stops by itself at `c2` if the build number does not move.
- **C. B, then `engine` and `seats`.** The only route to a build that is a fraction of today's for the changes that
  are most of the commits. It is a larger piece of work than all the rest, and it depends on a number nobody has yet: what
  share of commits change a contract.

What would change the plan: a `cargo test --no-run` after the `c2` edit that does not fall under half; a contract-change
share over 35%; or a second look at release (not incremental, 79% of cold for one edit) as a build problem separate from
structure, which `robustness-077` and `-081` already own.
