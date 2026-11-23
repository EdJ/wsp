# A state core for wsp

Plan `wsp-483`, 2026-11-08. **This is a draft for Ed to discuss, not a design he has approved.** It changes no code
and files no rows. The only settled points are the four decisions Ed took on 2026-11-07, listed below. Everything
else is a recommendation with its options beside it. The open points are the numbered questions in section 10.
Execution starts after the current pass (fleet-fixes).

Ed, 2026-11-07: *"it's clear we're using OS features within wsp to manage the work and status - let's consider a pass
on starting to move to a simple datastore and various state machines to manage the work"*.

**Settled (Ed, 2026-11-07).**
- **D1.** Start with SQLite and assume a move to Postgres or similar later. The SQL need not be portable. What
  must hold is the contract: a virtual layer in domain terms that no SQL, connection, transaction or row type
  crosses, with one conformance suite every backend passes. `wsp-store-contract` is the seam to grow. Leases,
  fencing tokens and idempotency keys belong to the contract's semantics, not to SQLite's single writer. On SQLite,
  several instances means one writer holding a lease, with standbys. Active-active waits for Postgres.
- **D2.** SQLite as a dependency is accepted. Its build is cached.
- **D3.** Components phase 2 is folded into this plan.
- **D4.** No singleton. Multi-tenancy is by instancing. Execution comes after fleet-fixes.

**Short answer.** The target proposed to Ed holds up, with four corrections.
1. **"The CLI is a thin client" is about state, not work.** `throttle run`, `check`, `verify` and `land` do cargo and
   git work in a tree, and they cost 66% of all CPU measured (section 1.5). That work stays where the tree is. What
   moves to the server is the record of it.
2. **"wsp stops calling ps/lsof" is right for seats, wrong for machines.** Load, disk, memory and power (`df`,
   `pmset`, `sysctl`, `memory_pressure`) are facts about a machine. They are not inferences about a seat. The
   executor reports them once a tick, the server stores them, and no CLI call asks the OS again.
3. **The largest probe is git, not ps.** A daemon tick runs about 143 `git` children against 34 `ps` (section 1.5).
   The store commits every write to `~/wsp`: 1,701 commits in the last 24 hours, 29,613 in all. The event table
   replaces that history as the audit trail. Git keeps only the text Ed edits (section 2.5).
4. **Events in commit order is easy on SQLite and hard on Postgres.** A Postgres sequence hands out ids in the order
   transactions start, not the order they commit. "Subscribe from event id N" therefore needs its own design before
   the second backend. The contract already says what must hold (section 8.4).

The migration is six steps (section 7). Step 1 alone removes compound's host polling. On today's figures that polling
is 3,707 calls and 1,992 CPU-s in 4½ hours, 15% of all of wsp's CPU.

---

## 1. Inventory: what wsp reads and writes today, and what it asks the OS

### 1.1 How it was measured

`wsp calls` (wsp-481, landed; wsp-488 split polls out) records every wsp invocation and every daemon tick in
`calls.jsonl`. The figures here cover the file's whole span: 2026-11-07T21:34Z to 2026-11-08T02:08Z (4h34m), with
**7,078 calls, 12,974 CPU-s and 352 daemon ticks**. They were taken with the fleet running two lists.
`wsp calls --by verb|child|caller|phase` reproduces them. Per-tick child counts come from the `"k":"tick"` lines'
`children` field, averaged over the 352 ticks.

Counts of source sites are from `grep` over `src/` and `crates/` at `47126dc`. Lines inside `#[cfg(test)]` modules
are left out unless marked.

### 1.2 The store, `~/wsp`

| part | size | format | writer | readers |
|---|---|---|---|---|
| `tasks/` | 2,288 files, 23.6 MB | Markdown with YAML front matter (`id`, `status`, `claim`…), sections `## Overview`, `## Details`, `## Decisions`, `## Verification`, `## Checks`, `## Legs`, `## Log` | `Store::save_task` (crates/store/src/store.rs:1397), 22 callers per components.md §3 | everything, via `Store::tasks` (crates/store/src/store.rs:1048), re-read by mtime and indexed in `~/.local/state/wsp/index/tasks.json` (crates/store/src/store.rs:1088) |
| `worklists/` | 101 files, 4.9 MB | Markdown: `## Overview`, `## Groups`, `## Decisions`, `## Log` (crates/store-contract/src/lib.rs:85) | worklist verbs, `cycle`, `barrier_run` | `worklist`, `cycle`, every surface |
| `projects/` | 45 files | Markdown + front matter | project verbs | every verb that resolves a row |
| `machines/`, `packets/`, `hooks/`, `archive/` | 2, 4, 1, 112 files | Markdown / JSON | machine, packet and project verbs | spawn, brief |
| `.git` | 29,613 commits, 1,701 in 24 h | git | `Store::commit` (crates/store/src/store.rs:3409) after each save | people; `git log` in rows' evidence |

### 1.3 Files under `~/.local/state/wsp`

There are 42 live files and 23 directories (plus one `.bak`). `Store::files()` (crates/store/src/store.rs:486) lists
23 of the files. The rest are opened by name in the module that owns them. The **class** column shows where each
one goes in this plan:
- **S**: domain state. It moves into the store.
- **E**: a log. It becomes events.
- **C**: a cache or an inference. It is deleted once the state it guesses at is recorded.
- **X**: executor- or machine-local. It stays on the machine.
- **U**: wsp's own surface (panel) state. It is client-side and never the server's.

| file | size | writer (verb, site) | readers (modules) | class |
|---|---|---|---|---|
| `agents.json` | 237 KB | `set_agent` store.rs:2289 (cmd_agent, cmd_govern, cmd_mandate, ending) | `agents_held` store.rs:2271 (11 modules) | S: agent ↔ seat |
| `bindings.json` | 3.6 KB | `set_binding` store.rs:2197, `clear_binding` :2216, `reap_bindings` :2852 (16 modules) | `bindings` store.rs:2190 (23 modules) | S: seat → row |
| `claims.json` | 4.8 KB | `set_claim` store.rs:2785, `clear_claim` :2792, `reap_claims` :2805 (14 modules; reap only from daemon) | `claims` store.rs:2244 (26 modules) | S: row → seat, lease |
| `governors.json` | 32 KB | `set_governor` store.rs:2421, `clear_governor` :2430, `edit_governor` :2457 (14 modules) | `governors` store.rs:2414 (22 modules); also read raw at crates/base/src/calls.rs:545 to name a caller's role | S: governor seats |
| `spawned.json` | 8 KB | `set_spawned` store.rs:2996, `reap_spawned` :3010 (cmd_agent, cmd_spawn) | `spawned` store.rs:2986 (cmd_agent, cmd_spawn, sync) | S: seat birth |
| `seat-since.json` | 19 KB | `note_seat_states` store.rs:3041 (cmd_agent, on every `wip`) | same, cmd_agent | C: seat state's start, inferred by sampling |
| `seat-memory.json` | new (`wsp-066`) | `memory::sample` / `sample_rss`, from the daemon's tick once a minute | `memory::seat_memory` (cmd_report: effort.json's `standing_seats`) | X → E: a seat's resident memory over its seated time, sampled; the executor's per-tick RSS replaces the `ps` |
| `said.json` | 169 KB | `set_said` store.rs:2908, `set_sentence` :2933 (cmd_agent); place_super.rs:365, place_compound.rs:239 per seat | `said` store.rs:2897 (14 modules) | C → S: a seat's last word |
| `worked.json` | 569 KB | `set_worked` store.rs:2833 (cmd_agent) | `worked` store.rs:2826 (cmd_agent, cmd_spawn, cmd_task, detail) | S: row ↔ seat history |
| `watches.json` | 1.5 MB | `update_watch` store.rs:2749, `set_watch` :2773 (attention, cmd_watch, wake, repair, cmd_spawn) | `watch`/`watches` store.rs:2716–2729 (10 modules, place backends) | S + C: waits on seats |
| `messages.json` | 826 KB | `save_message` store.rs:2669, `forget_message` :2688 (8 modules) | `messages`/`message` store.rs:2643–2653 (13 modules) | S: asks, notes, acks |
| `flags.json` | 2 B | `set_legacy_flag` store.rs:2560, `clear_flag` :2551 | `flags` store.rs:2543 | S (legacy, empty) |
| `mandates.json` | 0.5 KB | `set_mandate` store.rs:2373 | `mandates` store.rs:2366 | S |
| `handovers.json` | 2 B | `set_handover` store.rs:2499 (cmd_spawn, stand_down) | `handovers` store.rs:2492 | S |
| `handover.log` | 16 KB | `handover_log` store.rs:2513 | people | E |
| `pins.json` | 61 B | `set_pin` store.rs:3110 | `pins` store.rs:3098 (7 modules) | S |
| `machines.json` | 0.4 KB | `set_machine_live` store.rs:3184 (far, tunnel, cmd_machine…) | `machines_live` store.rs:3170; place_herdr.rs:1387 | S: machine reachability |
| `daemon.json` | 53 B | `set_daemon_holder` store.rs:2173 (daemon) | `daemon_holder` store.rs:2162 (cmd_agent, daemon, wake) | S → lease: today's singleton marker, last writer wins |
| `capped.json` | 2 B | `set_capped` store.rs:785 (cycle, room) | `capped` store.rs:778 | S |
| `starts.json` | 0.6 KB | `update_starts` store.rs:805 (cycle, starts) | `starts` store.rs:798 | S: start attempts |
| `pickup.json` | 4.4 KB | `update_pickup` store.rs:772 (pickup) | `pickup` store.rs:768 | S |
| `resumable.json`, `resume-held.json` | 48 B, — | `set_roster` store.rs:3229, `set_held` :3252 (cmd_resume, sync) | `roster` :3218, `held` :3245 | S |
| `closing.json`, `closing-flags.json` | 2 B, 140 B | src/closing.rs:275, :633 | closing | S |
| `finished-wip.json` | 28 B | src/ending.rs:424 | ending | C |
| `typed-unsent.json` | 2 B | src/repair.rs:624 (`unsent_tick` :637) | repair | **C: an order typed but not sent, read off the screen** |
| `first-orders.json` | 2 B | src/repair.rs:773 | repair | **C: a first order owed to a new seat** |
| `stuck-commands.json` | 2 B | src/repair.rs:676 | repair | C: a seat's command that waits on itself |
| `leaked-agents.json` | 2 B | src/leaked_agents.rs:27 | leaked_agents | C: agents with no seat, from `ps` |
| `reap-seen.json` | 2 B | src/cmd_role.rs:548 | cmd_role | C |
| `events.jsonl` | 9.2 MB | `log_event` store.rs:3322 (25 modules) | `events_of` store.rs:3309 (attention, cmd_agent, cmd_resume); read whole at src/timeline.rs:603, src/cmd_report.rs:554, src/cmd_cost.rs:120 | **E: already an event log, unindexed** |
| `cycle.log` | 4.1 MB | `log_line` store.rs:3644, src/cycle.rs:403 | src/cycle.rs:1317, src/timeline.rs:619; people | E |
| `attention.log` | 1.7 MB | attention (`kind_attention`) | people | E |
| `calls.jsonl`, `polls.jsonl` | 2.2 MB | crates/base/src/calls.rs:54, :56 | `wsp calls` | X: per machine; also reported as events |
| `spent.jsonl` | 0.5 MB | crates/wire/src/place_super.rs:265 | src/cmd_burn.rs:282 | E: spend |
| `daemon.log` | 3.5 MB | crates/wire/src/launchd.rs:162 | people | X |
| `throttle.json`, `hold.json` | 187 B, 11 B | crates/trees/src/throttle.rs:184, :499 | throttle | X: machine load policy |
| `disk.json` | 133 B | src/disk.rs:178 | disk | X |
| `panels.json`, `panel-view.json`, `detail.json`, `surface-frame.json` | 2.8 KB, 0.7 KB, 1.3 KB, 3.9 KB | src/panel/install.rs:187, src/panel/shared.rs:57, src/detail/mod.rs:91, src/panel/surface.rs:388 | panel, detail | **U** |

| directory | contents | owner (site) | class |
|---|---|---|---|
| `compound-seats/<seat>/` | 310 files: `seat.json`, `said.json`, `hooks.jsonl`, `burn.json` per seat | crates/wire/src/place_compound.rs:237, :255; place_super.rs:362, :369 | **X + C**: compound-sup's record of its session; wsp reads `hooks.jsonl` for turns (place_compound.rs:1875) |
| `barrier/`, `checks/` | 121k and 115k files, 37 GB and 34 GB: trees and logs of barrier and check runs | src/barrier_run.rs:299, src/check.rs:508 | X (trees); verdict text is S |
| `build/`, `warm/`, `cold/`, `trees/`, `scratch/`, `sandbox/` | build trees, warmed `target/`, cold runs, far tree copies, per-row scratch, sandboxes | crates/trees/src/cmd_verify.rs:296, src/disk.rs:123, src/cold.rs:119, crates/trees/src/treecopy.rs:457, src/scratch.rs:45, src/cmd_sandbox.rs:392 | X |
| `building/`, `builds/`, `full-suite/`, `shims/`, `greens/` | throttle slots, suite runs, cargo shims, green test lists | crates/store/src/sharing.rs:263, crates/trees/src/throttle.rs:618, :798, :747, crates/trees/src/greens.rs:72 | X |
| `verdicts/`, `batons/`, `briefs/`, `packets/`, `packet/` | verifier verdicts, leg batons, briefs, packets | src/closing.rs:167, :71, src/cmd_spawn.rs:193, src/verifier_packet.rs:239, src/packet_gen.rs:430 | S (text referenced by rows) |
| `attention/` | attention hook spool | src/kind_attention.rs:122 | E |
| `index/` | `tasks.json`, the store's mtime index | crates/store/src/store.rs:1088 | C: becomes the store itself |
| `sock/` | ssh control sockets per far machine | crates/wire/src/tunnel.rs:425 | X |
| `log/` | `daemon.log`, `daemon.pid` | launchd | X |

**What the table says.** About 25 files are domain state kept as whole-file JSON maps. Each is rewritten with an
`fsync` under one store-wide lock (`update_json` crates/store/src/store.rs:2085, the lock at :2024), so no two files
can change in one transaction. That is how a seat's binding, its claim and its governor record drift apart.
Eight files are caches of inferences (class C). Three are logs that already are events (`events.jsonl`, `cycle.log`,
`attention.log`), and nothing indexes them, so readers parse 9 MB to answer one question.

### 1.4 OS probes: call sites

| probe | site (non-test) | what it decides | caller (verb / tick phase) |
|---|---|---|---|
| `ps -A -o pid,ppid,command` | crates/base/src/base/procs.rs:20 (`processes`) | process tree for ownership | src/daemon.rs:409, :454, :480 (daemon self-checks); src/leaked_agents.rs:160 (`sweep:leaked-agents`); crates/trees/src/check_tree.rs:224, :250 (`sweep:check-trees`) |
| `ps -A -o pid,ppid` | crates/base/src/base/procs.rs:39 (`parents`) | ancestors to spare | crates/trees/src/check_tree.rs:494 (`ending:checks`) |
| `ps -A -o pid,ppid,etime,command` | src/waiting.rs:355 (`Shells::read`) | a seat's shells still running | every `wip`, `worklist ls --json` |
| `ps -A -E -o pid,ppid,rss,command` | src/memory.rs:145 | memory per seat | `wip` (src/cmd_agent.rs:4244), `cmd_watch` (src/cmd_watch.rs:4321) |
| `ps -o pid,state -p …` | crates/base/src/util.rs:1541 (`alive`) | is a recorded pid alive | place backends' `state` (place_compound.rs:1854), most verbs |
| `ps -o pid -p …` | crates/store/src/sharing.rs:390 (`alive`) | build slot holders alive | throttle |
| `ps -o lstart -p` | crates/store/src/checks.rs:144 | a check's pid is the same process | `ending:checks` |
| `ps -A -o pid,ppid,pgid` | crates/wire/src/place_super.rs:844 (`process_rows`) | a seat's process tree | `Place::pids`, despawn |
| `ps -A -o pid,ppid,comm` | crates/trees/src/check_tree.rs:327 (`exes`) | dev tools in a finished tree | `sweep:check-trees` |
| `ps -o rss -p` | crates/trees/src/check_tree.rs:520 | memory of strays | `sweep:check-trees` |
| `ps -A -o …` | src/cmd_role.rs:348 | processes for `reap` | `wsp role reap` |
| `lsof -d cwd -Fpn` | crates/trees/src/check_tree.rs:221 (`running_among`), :507 (`left_in_finished`) | **who owns a tree, by cwd** (wsp-478) | `sweep:check-trees`, `wip --json` |
| `lsof` | src/cmd_role.rs:616 (`reap`) | cwds for reap | `wsp role reap` |
| `kill -0` | src/check.rs:608, crates/trees/src/gc.rs:54, crates/trees/src/throttle.rs:596 | liveness | checks, gc, throttle |
| `kill -s KILL/TERM` | src/check.rs:681, :690; src/cmd_role.rs:574; crates/trees/src/check_tree.rs:259; crates/wire/src/place_super.rs:886; crates/trees/src/at_stop.rs:94; src/leaked_agents.rs:126; crates/wire/src/place_compound.rs:1232; src/cmd_sandbox.rs:501 | stops | sweeps, despawn, check timeouts, delivery fallback |
| compound screen render (`compound-render`) | crates/wire/src/place_compound.rs:746 (`read_screen`), used by `typed` :1824 → `composer` :385 and the trust prompt :743 | **is an order typed but unsent** | src/repair.rs:650, :873 (`repair:unsent`), src/untaken.rs:131, src/cmd_spawn.rs:1754, :1774, :1822, :3025 |
| herdr `pane.read` | src/cmd_agent.rs:5458 (`peek`); herdr `agent.list`/`pane.list` crates/wire/src/place_herdr.rs:825, :861 | herdr seat state | `peek`, census |
| hook log read | crates/wire/src/place_compound.rs:1875 (`turn_began_since`, `hooks.jsonl`) | did a turn start (answers an order) | `repair`, `cycle` |
| transcript read | src/agent_commands.rs:2723 (`transcript`), :2699; src/cmd_agent.rs:2303; src/did.rs:463 (`of_transcript`); src/cmd_verifications.rs:347; src/timeline.rs:519 | what a seat did, its spend, its last word | `ending:verification`, `wip`, `attempts`, `timeline`; 35 files mention transcripts, 24 read a `.jsonl` |
| `launchctl` | crates/wire/src/launchd.rs (12 sites) | is the daemon loaded | install, wip |
| `ssh` | crates/wire/src/tunnel.rs, far.rs (15 sites) | far machines, beats (far.rs:780) | `tunnels` phase every tick |

### 1.5 What it costs

**By verb (4h34m, 7,078 calls, 12,974 CPU-s).**

| verb | calls | p50 ms | CPU-s | share | children |
|---|---|---|---|---|---|
| `throttle run` | 231 | 41 | 6,717 | 51.8% | cargo 231× — work, not state |
| `worklist advance` | 119 | 9,007 | 2,056 | 15.8% | git 13,150×, ps 1,899×, compound-render 1,190× |
| `check` | 133 | 1 | 907 | 7.0% | sh, git — work |
| `wip --json` | 299 | 1,597 | 461 | 3.6% | ps 1,685× (218 s), compound-render 2,368×, lsof 135× (51 s), df 4,784× |
| `worklist ls --json` | 254 | 1,538 | 425 | 3.3% | ps 4,974×, compound-render 3,238× |
| `stamp --json` | 2,059 | 216 | 418 | 3.2% | compound-render, ps |
| `worklist show --json` | 253 | 995 | 303 | 2.3% | |
| `spawn` / `despawn` | 66 / 65 | 7,201 / 1,437 | 278 / 172 | 3.4% | |

**By caller.** compound's host (`none via host`) made **3,707 calls for 1,992 CPU-s (15.4%)**, all polls of read
verbs. Its children were compound-render 20,592×, ps 8,822× (419 s) and lsof 134×.

**By child, all callers.** git ran 29,336× (1,575 s wall), compound-render 24,736× (772 s), ps 14,853× (625 s),
df 5,718×, lsof 158× (63 s at p50 399 ms).

**Per daemon tick (352 ticks).** The whole tick takes p50 2.2 s and p95 54 s. On average a tick runs **143 git,
43 compound-render, 34 ps, 8 df, 1 ssh, 0.4 lsof and 0.4 kill**. The dearest phases are `cycle` (p95 34 s),
`ending:verification` (p95 14 s, transcripts), `sweep:check-trees` (p50 2.5 s, ps + lsof) and `sweep:scratch`.

### 1.6 The inferences, by kind

Each incident in the packet is one of five inferences. The machines in section 3 retire them.

| # | inference | how it is made today | incident |
|---|---|---|---|
| I1 | **who owns a process** | cwd under a tree (`lsof`), or descent from a recorded pid (`ps`) | wsp-478: the herdr server TERMed |
| I2 | **is a seat alive, and in what state** | pid alive (`ps -p`), herdr `agent.list`, compound's record + `ps`, `seat-since.json` sampled on each `wip` | wsp-480: 70 `ps` per `wip --json` |
| I3 | **was an order delivered and taken** | a screen render (`composer`), `hooks.jsonl` turn starts, `typed-unsent.json`, transcripts | a seat stuck 6 min in its own wait loop; orders "typed but unsent" |
| I4 | **has a seat ended** | the absence of a pane/process at the next census | wsp-411: a second idle seat after a re-plan |
| I5 | **what a row's state is** | front matter + `## Verification` + `claims.json` + `bindings.json` + `governors.json`, joined per call | the joins in `wip_rows` (src/cmd_agent.rs:4616) and `runs` (:4572) |

---

## 2. The data model and the store contract

### 2.1 Entities

| entity | key | holds | today in |
|---|---|---|---|
| `Tenant` | instance name | one store, one socket, its executors | `WSP_STATE` (a sandbox is one already, crates/store/src/store.rs:2154) |
| `Principal` | `person:ed`, `seat:cpd-3698`, `executor:eds-macbook-i5`, `wsp:<instance>` | who did it | `calls.rs`'s role guess |
| `Project`, `Row`, `List`, `Group` | `wsp`, `wsp-483`, `state-core`, `state-core/2` | status, fields, membership | `~/wsp/projects`, `tasks`, `worklists` |
| `Seat` | `cpd-3698` | executor, machine, kind, model, state, the row it serves | `bindings`, `spawned`, `agents`, `governors`, `compound-seats/` |
| `Claim` | row id | the seat, lease expiry, fencing token | `claims.json` |
| `Order` | order id | from, to (seat), text ref, state, attempts | `messages.json`, `said.json`, `typed-unsent.json`, `first-orders.json` |
| `Ask` / `Note` / `Flag` | message id | raised, acked, answered | `messages.json`, `flags.json` |
| `Run` | (row, attempt, kind) | a check, a verification, a barrier run: state, verdict, log ref | `## Verification`, `## Checks`, `barrier/`, `verdicts/` |
| `Executor` / `Machine` | machine name | transport, lease, last beat, load facts | `machines.json`, `~/wsp/machines` |
| `Lease` | name (`tick`, `barrier:<list>/<g>`, `sweep:<kind>`, `deliver:<seat>`, `claim:<row>`) | holder, token, expires | `daemon.json` (one marker) |
| `Event` | id (monotonic) | at, principal, kind, subject, payload | `events.jsonl`, `cycle.log`, git log |

### 2.2 The contract's operations, in domain terms

The contract grows `wsp-store-contract` (crates/store-contract, today 99 lines of constants) into types and one trait
family. No SQL, connection, transaction handle or backend row type appears in it (D1). These are domain verbs. A
backend chooses how to make each one atomic.

- **Reads.** `row(id)`, `rows(filter)`, `seat(id)`, `seats(filter)`, `list(id)`, `orders_for(seat, states)`,
  `claim(row)`, `lease(name)`, `machine(name)`, `snapshot() -> (State, EventId)` (the whole domain state and the
  id it is consistent at).
- **Events.** `events_after(EventId, limit) -> Vec<Event>` and `head() -> EventId`. A wake-up primitive,
  `wait_for(after: EventId, timeout)`, which a backend implements with a condition variable (SQLite, in-process),
  a file watch, or `LISTEN` (Postgres).
- **Commands.** `apply(cmd: Command, key: IdempotencyKey, by: Principal) -> Result<Applied>`. One command is
  validated by its machine (section 3), and its state change and its events are written atomically. `Applied`
  carries the new events' ids. A second `apply` with the same key returns the first result and writes nothing.
- **Leases.** `take(name, holder, ttl) -> Option<Token>`, `renew(name, token, ttl) -> bool`, `release(name, token)`.
  A token is a strictly increasing integer per name. `take` succeeds only if the lease is free or expired, and it
  then issues `token = last + 1`.
- **Fenced writes.** Every command issued under a lease carries `Fence { lease, token }`. `apply` refuses it with
  `Stale` when the lease's current token is greater. This is how an executor or an instance that lost its lease
  and has not noticed yet cannot write.
- **Claims.** `claim(row, seat, ttl, fence) -> Token` and `release(row, token)`. A claim is a lease named
  `claim:<row>`, so the same semantics and tests cover it.
- **Text.** `text(row, section) -> (String, Rev)` and `set_text(row, section, String, expect: Rev)`, for the prose
  that stays in files (section 2.5).

### 2.3 The conformance suite

The suite is one test crate, `wsp-store-conformance`, generic over `impl Store`. It lives under `crates/` and runs
against every backend. A backend is done when it passes, with no exceptions file. The cases:
1. **Atomicity.** A command that touches a seat, a claim and a row either lands entirely or leaves nothing and no
   event.
2. **Idempotency.** The same key applied twice, concurrently from two handles, yields one change, one set of events
   and the same `Applied`.
3. **Lease exclusivity.** N handles race `take`, and exactly one wins per expiry window. Tokens strictly increase
   across take-overs.
4. **Fencing.** A write under token *t* after a take-over to *t+1* is refused, and nothing it would have written is
   visible.
5. **Event order.** `events_after(n)` returns events in commit order with no gaps a reader can skip. A reader that
   saw id *m* never later receives an id less than *m* that it did not see (the hard case, section 8.4).
6. **Snapshot consistency.** `snapshot()`'s state equals the fold of all events up to its id.
7. **Wake-ups.** `wait_for(after)` returns within a bound after a commit from another handle.
8. **Crash.** Kill a handle mid-`apply` (a test hook) and confirm the result is all-or-nothing on reopen.
9. **Machine guards.** Every illegal transition in section 3's tables is refused by `apply` with its named reason,
   and no event is written.
10. **Text.** `set_text` with a stale `Rev` is refused (no lost edit of Ed's prose).

How a backend runs them (wsp-047): it implements `wsp_store_conformance::Harness` (open a store with a domain, open
more handles on it, arm the crash hook, reopen). Each case is a `pub fn case_x<H: Harness>(&H) -> CaseResult` in
`wsp_store_conformance::cases`, and a backend's tests call them one by one, `case_crash(&harness).unwrap()`, one
`#[test]` each; a broken guarantee is an `Err(CaseFailure)` naming the case and what it saw. The suite's `MemStore`
passes them first, in the crate's `tests/mem.rs`, and each of its `Knobs` breaks one guarantee to show the matching
case fails (ignored idempotency key: 2; skipped fence: 4; a gap in event ids: 5; half a command written: 8). Case 9
runs on the suite's `TestDomain` and its illegal transitions, staged until `wsp-machines` exists; it then takes
section 3's tables.

### 2.4 The SQLite schema behind it (sketch)

The SQL need not be portable (D1). This sketch shows that the contract fits SQLite and costs little. It is not the
schema to build.

```sql
PRAGMA journal_mode = WAL;                    -- readers never block the writer
CREATE TABLE event (
  id        INTEGER PRIMARY KEY,              -- rowid: commit order under one writer
  at        TEXT NOT NULL, principal TEXT NOT NULL,
  kind      TEXT NOT NULL,                    -- 'seat.started', 'order.acknowledged', …
  subject   TEXT NOT NULL,                    -- 'seat:cpd-3698', 'row:wsp-483'
  payload   TEXT NOT NULL,                    -- JSON
  workspace TEXT NOT NULL DEFAULT 'private'   -- 'private' or 'shared:<name>' (schema 2; an older event reads private)
);
CREATE INDEX event_subject ON event(subject, id);
CREATE TABLE command (key TEXT PRIMARY KEY, principal TEXT, at TEXT, result TEXT);   -- idempotency
CREATE TABLE lease   (name TEXT PRIMARY KEY, holder TEXT, token INTEGER NOT NULL, expires TEXT);
CREATE TABLE row     (id TEXT PRIMARY KEY, project TEXT, state TEXT, fields TEXT, text_rev TEXT);
CREATE TABLE list    (id TEXT PRIMARY KEY, state TEXT, fields TEXT);
CREATE TABLE grp     (list TEXT, n INTEGER, state TEXT, fields TEXT, PRIMARY KEY (list, n));
CREATE TABLE seat    (id TEXT PRIMARY KEY, executor TEXT, machine TEXT, kind TEXT, state TEXT,
                      row TEXT, since TEXT, fields TEXT);
CREATE TABLE claim   (row TEXT PRIMARY KEY, seat TEXT, token INTEGER, expires TEXT);
CREATE TABLE ord     (id TEXT PRIMARY KEY, seat TEXT, state TEXT, attempts INTEGER, text TEXT, fields TEXT);
CREATE TABLE message (id TEXT PRIMARY KEY, kind TEXT, state TEXT, fields TEXT);
CREATE TABLE machine (name TEXT PRIMARY KEY, transport TEXT, beat TEXT, facts TEXT);
CREATE TABLE run     (row TEXT, attempt INTEGER, kind TEXT, state TEXT, verdict TEXT, PRIMARY KEY (row, attempt, kind));
```

Every `apply` is one `BEGIN IMMEDIATE … COMMIT`. It checks the command key, checks the fence, writes the state
rows and appends to `event`. The state tables are a projection of `event` kept in the same transaction, so case 6
of the suite holds by construction, and a rebuild from `event` is a test, not a recovery path. Retention is an open
question (Q7). Today's `events.jsonl` is 9 MB after months, so all of it fits.

### 2.5 What happens to `~/wsp/tasks`

| option | what | for | against |
|---|---|---|---|
| **A. Text canonical, store indexes** | Files stay the truth; the server watches them and keeps SQLite as an index | Ed edits as now; nothing to migrate | Two sources of truth for status, claims, verification, log; every write is still a file rewrite + git commit (1,701/day); machines cannot be atomic across a file and the DB |
| **B. Moved in** | Every row lives in SQLite; `wsp show`/`edit` render and parse | One truth; atomic; no git churn | Ed loses `vim`/`grep`/git on rows; a big migration; the prose is the part people read |
| **C. Split by who writes it (recommended)** | Prose that people write (`## Overview`, `## Details`, `## Decisions`, project handbooks, list overviews) stays in Markdown files under git, read through `text()`/`set_text()` with a revision check. Everything a machine writes (`status`, `claim`, `## Verification`, `## Checks`, `## Legs`, `## Log`, groups' states) moves into the store. `wsp show` joins them | Ed's text stays text; git history keeps what is worth reading; machine state is atomic; store commits fall to the edits people make | The file loses its machine sections, so a `cat` of a row is no longer the whole row (an export, `wsp show --md`, gives it back); one migration of sections into tables |

Under C, a row's `## Log` becomes the row's event stream (`subject = row:<id>`), and `wsp show` renders it as
today. The R1 rule ("one writer per relation", components.md §3) carries over: each machine-written section has
one machine as its writer.

---

## 3. The machines

Each machine is a pure function `step(state, event) -> Result<(state', effects), Refusal>` in a crate with no IO
(section 6). The server applies it through `Store::apply`. Every transition writes an event with its principal.
Every refusal is returned with a named reason, and an illegal transition is a logged refusal, never a silent
no-op. **Effects** (spawn, deliver, stop, run a check) are commands to an executor or a worker. Their results come
back as events, never as a re-read of the OS.

### 3.1 Orders (seed: compound-993 and wsp-477's deliver/acknowledge)

| state | → on event | guard | removes |
|---|---|---|---|
| `queued` | `deliver` (by the holder of `deliver:<seat>`) → `typed` | seat is `idle` or `working` and accepts orders; one order in `typed` per seat | |
| `typed` | executor `delivered` → `delivered` | fence matches the seat's executor lease | `composer` screen render (place_compound.rs:1824), `typed-unsent.json` (repair.rs:624) |
| `typed` | executor `refused` / timeout → `queued` (attempts+1) or `failed` at the cap | | `repair:unsent` retyping (repair.rs:637) |
| `delivered` | executor `acknowledged` (the seat's turn began on this order id) → `acknowledged` | the ack names this order's id | `turn_began_since` over `hooks.jsonl` (place_compound.rs:1875), transcript reads |
| `acknowledged` | seat `idle` after it → `done` | | the 6-minute wait loop: the waiter subscribes to `order.done` |
| any | `cancel` → `cancelled` | not `done` | |

The `first-orders.json` file (repair.rs:773) becomes "a `queued` order exists for a seat in `started`" and needs no
file of its own.

### 3.2 Seats

| state | → on event | guard | removes |
|---|---|---|---|
| `requested` | executor `claimed` the spawn → `spawning` | executor holds a lease; idempotency key = (row, attempt) | double spawns on retry |
| `spawning` | executor `started` (agent pid recorded *by the executor*) → `started` | | 150 ms `state` polling in spawn (place.rs:1158) |
| `started` | first order `acknowledged` → `working` | | |
| `working` ⇄ `idle` | executor `turn.began` / `turn.ended` (from compound-sup's hooks) | | `seat-since.json` sampling (store.rs:3041), herdr `agent.list` state guesses |
| any live | `end` requested → `ending` | the requester holds the row's claim or is the person | |
| `ending` | executor `stopped` (with exit and the pids it ended) → `gone` | | wsp-411: the end is recorded, not inferred from absence; I4 |
| any live | executor lease expired (no beat) → `lost` | | census-by-absence (place.rs:1237) |
| `lost` | beat returns → previous state; or person `despawn --lost` → `gone` | | remote-agents v1 §3's held seat |

Ownership of processes (I1) belongs to the executor, which started them and keeps the tree. wsp asks "stop
seat X". It never asks "who is in this directory", which retires `lsof -d cwd` (check_tree.rs:221, :507) and the
`ps` walks in the sweeps.

### 3.3 Rows

| state | → | guard | removes |
|---|---|---|---|
| `todo` | `claim` → `doing` | claim lease taken; a seat named | joins of `claims.json` + `bindings.json` per call (I5) |
| `doing` | `review` → `review` | claim token current; a commit recorded | |
| `review` | verifier `holds` → `done` (landed) / `blocks` → `sent_back` | verifier holds `verify:<row>` lease; landing is an effect whose result is an event | |
| `sent_back` | `claim` → `doing` | | |
| any open | `block` → `blocked`; `unblock` → previous | | |
| any | `park` → `parked` | | |

`## Verification` (today written by verification.rs) becomes the `run` table plus `run.*` events. `wsp show` renders
it.

### 3.4 Lists and groups

| state | → | guard |
|---|---|---|
| list `planned` | `go` → `running` | groups defined |
| group `pending` | previous group `passed` → `running` | the `cycle` holder |
| group `running` | all members `done`/accepted → `barrier` | |
| group `barrier` | barrier run (lease `barrier:<list>/<n>`) `passes` → `passed`; `fails` → `held` | one barrier run per (list, group, attempt), key-protected |
| group `held` | person or governor `release` → `barrier` / `running` | |
| list `running` | last group `passed` → `finished` | |

### 3.5 What each machine retires, against the inventory

| inference (1.6) | retired by | probes and files deleted |
|---|---|---|
| I1 ownership | seats (executor owns the tree) | `lsof` ×2 sites, `ps` in check_tree (×4), leaked_agents, `reap-seen.json`, `leaked-agents.json` |
| I2 seat state | seats | `seat-since.json`, `util::alive` per seat, herdr state guesses, `memory.rs` per call (the executor reports RSS per tick) |
| I3 delivery | orders | `typed-unsent.json`, `first-orders.json`, `stuck-commands.json`, screen renders (24,736 in 4.5 h), `hooks.jsonl` scans |
| I4 ending | seats | census-by-absence, `finished-wip.json` |
| I5 row state | rows, lists | per-call joins, `index/tasks.json`, store git commits for machine sections |

---

## 4. The API and notifications

### 4.1 Shape

- **One socket per instance**: `$WSP_STATE/api.sock` (a unix socket, mode 0600). The framing is newline-delimited
  JSON. The transport is behind one trait (`Connection`), so TCP+TLS or a WebSocket later is a new impl, not a new
  protocol (step 6).
- **Envelope.** Every request carries `{ "v": 1, "id": <client req id>, "key": <idempotency key, commands only>,
  "by": <principal>, "verb": …, "args": … }`. The reply is `{ "id", "ok" | "err": {code, text}, "events": [ids] }`.
  `by` is mandatory from day one (one person now). The server checks it against the connection's credential. For a
  unix socket that is the peer uid plus a per-seat token the executor hands its agent. That makes multi-person work
  an auth and permissions change, not a protocol change.
- **The server knows only its domain**: rows, lists and groups, seats, orders, asks, machines. It has no notion of
  a client's views, layouts or selectors (Ed, 2026-11-07).

### 4.2 Verbs

| kind | verbs |
|---|---|
| read | `state` (snapshot + event id), `row`, `rows`, `list`, `lists`, `seat`, `seats`, `orders`, `asks`, `machines`, `events` (from id, bounded) |
| command | `row.add/edit/claim/review/block/park/land`, `list.go/hold/release`, `seat.spawn/end/despawn`, `order.send/cancel`, `ask.raise/answer/ack`, `machine.set` |
| stream | `subscribe { from?: EventId }` |
| executor | section 5 |

The CLI's ~100 verbs map onto these. Verbs that do work in a tree (`throttle run`, `check`, `verify`, `land`'s
rebase) run in the client and then report through a command, so the server records the result and never runs cargo
itself.

### 4.3 Subscribe (v1, deliberately small)

- One connection. `subscribe { from: N }` streams every event with id > N in commit order, then follows the head.
- `subscribe {}` (N absent) sends `{ "state": <snapshot>, "at": M }` first, then events from M. A snapshot too
  large for one line (16 MiB) comes as pieces of about 4 MiB, each `{ "part": <some of it>, "at": M }`, with
  `state` holding the rest. The pieces are disjoint, and taken in order they are the snapshot. A snapshot that
  fits in one line is still one `state`, so an older client reads it as before (wsp-702).
- On a reconnect or a gap the client asks again `from` its last id. If the server answers `too_old` (retention,
  Q7) the client takes `state` afresh. Ids are the de-duplication key, so delivery is at-least-once and the client
  drops an id it has seen.
- A plain heartbeat `{ "beat": <head id>, "at": … }` every 10 s. A client that misses three reconnects.
- **Not in v1** (named only as later options): per-view or filtered subscriptions, patches or diffs, server-side
  projections, fan-out to browsers. Each one would make the server aware of a client's shape, which Ed ruled out
  for v1.
- "Subscribe from event id N" sits on the contract's `events_after` and `wait_for` (section 2.2). It is the same
  operation on SQLite and Postgres.

### 4.4 How compound's host uses it

host-core's data layer is `request → store → select → write → poll` (compound docs/agents-reference.md, the
`host-core` row). Today `poll` runs `wsp wip --json` and its siblings every 3–4 s (crates/host-core/src/request.rs:105
in compound, per wsp-480). Under this plan:
- `request` opens the socket once and sends `subscribe {}` (later `subscribe {from}` with its last id).
- `store` holds host-core's own copy of the domain state, folded from the snapshot and then from each event.
- `select` is unchanged: every view is built by host-core's selectors from host-core's store. The server never
  sees a view.
- `poll` is deleted. A reconnect replaces it.

This is the change that removes the 3,707 calls and 1,992 CPU-s (1.5). It needs only the read path (step 1).

### 4.5 How the CLI uses it

- Read verbs (`wip`, `ls`, `show`, `worklist show`) send one request and render the reply. They do no OS probes and
  no store-file walks.
- Command verbs send one command with a fresh idempotency key (uuid). If the reply is lost, the CLI retries with
  the same key.
- Wait verbs (`wsp wait`, a seat's wait loops) subscribe and filter client-side for the events they wait on.
- `--json` output stays byte-compatible during migration, so compound and the agents do not change at the same
  time as wsp. Q5 asks whether that holds until step 4 or only step 1.

### 4.6 When the server is down

The daemon dropped twice after installs (wsp-449). Options:

| option | reads | writes | risk |
|---|---|---|---|
| a. Refuse loudly | `wsp: the wsp server is not running (launchd: …); wsp daemon --ensure` | same | the fleet stops, but visibly |
| b. Kick and retry | `launchctl kickstart` the agent, retry for ~5 s, then (a) | same | none extra; the CLI needs launchd knowledge (it has it, launchd.rs) |
| c. Read-only fallback | open the store read-only through the contract | refused | two read paths; must be the same code (the contract's reads) |
| d. Embedded writer | a CLI takes the writer lease itself | allowed | a CLI process becomes a server for a moment; contradicts "thin client" |

**Recommended: (b), then (c) for reads only.** A command never runs without a server, because only the server
holds the machines' leases. Reads fall back to the contract's read operations opened read-only, which SQLite WAL
allows next to a live writer, so `wsp wip` still answers while the person restarts the server. Every fallback read
prints one line saying the server is down. This is Q4.

---

## 5. The executor protocol

### 5.1 The messages

An executor is a process on one machine that starts and supervises agents (compound-sup). It connects to any
instance of its tenant's server.

| message | direction | carries | notes |
|---|---|---|---|
| `hello` | E → S | executor id, machine, transport, version, capacity, the seats it already runs | on (re)connect; the server reconciles its seats against this |
| `lease` | S → E | executor lease token, ttl | every executor write carries it (fencing) |
| `beat` | E → S | token, machine facts (load, disk, memory, power), per-seat `{state, rss, last_turn}` | every 30 s (today's `far::BEAT_SECS`, crates/wire/src/far.rs:780); replaces every per-call `ps`, `df`, `pmset`, `sysctl` |
| `claim` | E → S | a `seat.requested` it will start | idempotent on (row, attempt); one executor wins |
| `started` / `stopped` | E → S | seat, pid tree root, exit, the pids it ended | the executor ends what it started; nothing else signals |
| `order` | S → E | order id, seat, text | at-least-once; the executor de-duplicates by order id |
| `delivered` / `refused` | E → S | order id | from compound-sup's deliver verb (wsp-477) |
| `ack` | E → S | order id, turn start | from the hook that sees `UserPromptSubmit` for that order |
| `turn` | E → S | seat, began/ended | `working` ⇄ `idle` |
| `report` | E → S | spend, last word, transcript ref, copy-back manifest | replaces `said.json`, `burn.json`, `spent.jsonl` writes into wsp's state |
| `stop` | S → E | seat | |

The fencing rule: the server issues each executor a lease token on `hello`, and an executor that loses its lease
(missed beats past the ttl) gets a new token on reconnect. Any message carrying an older token is refused, so a
stale executor cannot re-report a seat that was reassigned while it was unreachable.

### 5.2 compound-sup locally and remotely

- **Locally**, compound-sup runs as one supervisor per machine (or keeps one per seat, behind one executor face;
  Q8) and connects to `api.sock` directly. wsp stops starting agents itself: `Place::open/start` become
  `seat.spawn` commands that an executor claims.
- **Remotely**, the same compound-sup connects through the ssh-forwarded socket that far seats use today (the
  `.sock` of remote-agents §2). The protocol is identical, which is what makes local and remote agents the same
  thing.
- **herdr panes** are not compound-sup. They can stay a second executor kind behind the same protocol (a thin
  adapter that reads herdr's own events), or herdr seats can be retired. That is Q9.

### 5.3 What of remote-agents v1 carries over

All of it, as the seed, not a thing to replace:
- **The beat and the lease** (remote-agents §3: beat every 30 s, `lease` default 180 s, a machine field) become
  `beat` and the executor lease, for local seats too. Today "a local seat hears nothing" (§3). Under this plan a
  local seat is beaten the same way.
- **The gate fails closed** (`compound-remote gate`, §3). It stays far-side. Its input becomes the executor lease.
- **Unreachable is a third state, never empty** (§1, `Census`, place.rs:1237). It becomes the seat state `lost` and
  the machine's `silent`.
- **The claim carries machine and lease, not an id suffix** (§4). That is the claim row in 2.4.
- **One door per far machine, far state in one `<exec_dir>`, copy-back keyed by manifest, the transport named in the
  machine record** (§4). These carry over unchanged. The `report` message carries the manifest.

---

## 6. Components: what replaces phase 2's cut lines

Phase 1 stays as it is: `wsp-base`, `wsp-testkit`, `wsp-store`, `wsp-store-contract`, `wsp-wire`, `wsp-trees`
(docs/components.md §3, "Where the cuts stand"). The phase-2 lines, `wsp-engine` (32.8k lines) and `wsp-seats`
(30.1k), are replaced by cuts along the server's own seams:

| crate | holds | depends on | writes |
|---|---|---|---|
| `wsp-store-contract` (grown) | domain types (`Row`, `Seat`, `Order`, `Event`, `Command`, `Fence`, `Token`, `Principal`), the `Store` trait family of 2.2 | `wsp-base` | nothing (types only, R2) |
| `wsp-store-conformance` | the suite of 2.3, generic over `impl Store` | contract, testkit | nothing |
| `wsp-store-sqlite` | the backend: schema, `apply`, leases, events | contract, `rusqlite` (bundled) | the database |
| `wsp-machines` | `step` for orders, seats, rows, lists/groups; guards; refusals | contract only, **no IO** | nothing (pure) |
| `wsp-api` | envelope, verbs, events on the wire, executor messages; `Connection` trait | contract | nothing (types only) |
| `wsp-server` | the daemon: socket, subscribe fan-out, leased once-only work (tick, barriers, sweeps, delivery), applying machines through the contract | api, machines, contract (not the sqlite crate, except in `main`) | through `Store` only |
| `wsp-client` | the CLI's connection, retry and idempotency keys, the read-only fallback | api, contract | nothing |
| `wsp` (bin) | the verb loop, rendering, tree work (`trees`, `wire` for git/cargo/throttle) | client, trees, wire, base | trees |
| executor | in compound (`compound-sup`), speaking `wsp-api`'s executor messages | `wsp-api` (published as a path or git dependency) | its machine |

**Where today's modules go.** Of `wsp-engine`: `cycle`, `repair`, `ending`, `legs`, `hold*` and `closing` split.
Their decisions become `wsp-machines` and their timing and leases become `wsp-server`. `worklist`, `cmd_task` and
`cmd_worklist` become client verbs. `barrier*`, `check` and `verification` keep their tree work in the client or
worker and record runs through commands. Of `wsp-seats`: `cmd_spawn`, `wake`, `room` and `agent_commands` lose their
OS halves to the executor and keep their decisions in `wsp-machines`. `cmd_agent`'s `wip_rows`
(src/cmd_agent.rs:4616) becomes a client-side render of `seats` + `rows`. The `Fleet` port of components §3 (p5) is
the executor protocol seen from inside wsp, so p5's work is not lost: its ~12 methods are the server's half of 5.1.

**What of wsp-474's measure still applies.** wsp-474 counts how many `crates/` commits change a `*-contract` crate.
It found 0 of 5 so far (row at review). That measure gated phase 2. It now guards the contract instead. **R5**
(a contract changes only by a row of its own) applies to `wsp-store-contract` and `wsp-api`. A share above ~35% after
step 2 means the domain types are not settled and the cut is churning. The response is to slow the migration, not
to cut further. The other gates of components §5 (pure moves, the test count before and after, `serde_json` as the
only external dependency) need one amendment: **D2 admits `rusqlite`** as the second external dependency, in
`wsp-store-sqlite` only.

---

## 7. Migration: the fleet on wsp throughout

Each step lands behind the previous one, the live fleet runs on every intermediate state, and each has a measure
taken with `wsp calls` before and after. **Baselines (1.5):** `wip --json` p50 1,597 ms and 1.54 CPU-s per call;
host polling 1,992 CPU-s per 4.5 h; per tick 143 git, 43 screen renders, 34 ps, 0.4 lsof; tick p50 2.2 s, p95 54 s.

**Amended 2026-11-16 (wsp-652, a draft like the rest).** Ed, 2026-11-15: *"I think we should start by segregating
local operations into an "agent manager" such that each agent host becomes fully delineated from wsp"*. The
executor seam that step 3 drew after the server now comes **first**, as steps **M0–M4** of docs/agent-manager.md
section 6. There is one agent manager per host, the Mac included, and wsp reaches each one over TCP. The manager
steps need no store and no server. Steps 0–6 then run on top of them, changed as the rows below say. The baselines
for M0–M4 are 2026-11-16's (agent-manager 6.1): per tick 37.5 ps, 59.5 renders and 24.6 df, tick p50 19.5 s, spawn
p50 9.4 s.

| step | what | measure: done when | rollback |
|---|---|---|---|
| **M0. herdr out, seam named** (agent-manager 6.2) | wsp-120 (D20). `host-api` types. Every OS-touching call outside tree work goes behind one in-process `Host` trait. | no probe outside the in-process host; herdr references → 0; spawn p50 no worse | revert (pure move) |
| **M1. The manager on the Mac** | The in-process host becomes a launchd process. wsp dials it on TCP. Hooks, journal and events go to it, and the daemon folds the events. | ps per tick → ≤ 1; renders → 0; `wip` < 0.2 CPU-s; wsp-411-kind → 0 | `WSP_HOST=inproc` |
| **M2. The i5 under its own manager** | The heartbeat replaces the tick-paced beat. Per-seat ssh forwards and the tunnel go. | ssh for install and tree bytes only; no wsp-639 flap; far manifest < 15 s | machine `transport: ssh` |
| **M3. Tree jobs under the manager** | check, build, verify and barrier processes run as manager jobs. check_tree's sweep and leaked_agents go. | lsof → 0; wsp-478-kind → 0 | jobs in-process behind a flag |
| **M4. Trees placed** (T2: Ed's answer to agent-manager Q6, confirmed by Jesper 2026-11-18, provisional pending Ed's confirmation; agent-manager 6.2, "M3 under T2") | checkout and land on the agents' hosts, with a fenced push to the trunk | a list on two hosts with no copies | T1 (rows on the Mac) |
| **0** (prep, could ride fleet-fixes' tail) | `rusqlite` behind the C-cached wrapper (section 9); the contract crate's types; the conformance suite against an in-memory fake | a cold tree builds SQLite in < 5 s from cache. **Met for SQLite's own build** (wsp-048, 2026-11-18): the `libsqlite3-sys` build script takes 1.2 s debug and 1.1 s release from cache. **Not met as the whole command's wall**: `cargo build -p wsp-store-sqlite` took 7.7 s debug and 6.1 s release at load 8–15 (section 9, "Step 0's result") | nothing to roll back |
| **1. Server and store, read path; compound subscribes** | The daemon opens SQLite and **mirrors** today's files into it each tick (a projection; files still the truth). `api.sock` serves `state`/reads/`subscribe`. compound's host subscribes; `wip --json` reads through the API | host polling calls → 0; `wip --json` < 100 ms wall, < 0.1 CPU-s; no `ps`/`lsof` in read verbs; daemon tick no slower. **Not met** (wsp-077, 2026-11-20): see "Step 1's result" below | the host goes back to `poll` (one flag); the CLI reads files |
| **2. Orders machine** | Orders become commands; delivery via compound-sup's deliver verb and ack (wsp-477) under a `deliver:<seat>` lease; `repair:unsent` deleted | screen renders per tick → 0; `typed-unsent`/`first-orders`/`stuck-commands` files gone; incidents of "typed but unsent" and wait-loop stalls → 0 over a fortnight | `repair:unsent` restored behind a flag |
| **3. Seats via executors, local and remote alike** | compound-sup speaks 5.1 locally and far; spawn/stop through it; beats carry machine facts; ps/lsof/cwd inference deleted (check_tree's `lsof`, `procs::processes` callers, `memory.rs` per call, `seat-since.json`) | `ps` per tick 34 → 0 (wsp side); `lsof` → 0; leaked/second-idle seats (wsp-411 kind) and wrong-owner kills (wsp-478 kind) → 0; spawn p50 7.2 s no worse | herdr/legacy `Place` backends stay compiled until the fortnight passes |
| **4. Rows and lists machines** | Section 2.5 option C: machine sections out of Markdown into the store; `cycle`/`ending`/`closing` as machines; store git commits only for prose | store commits/day 1,701 → people's edits only (expect < 100); `worklist advance` CPU (2,056 per 4.5 h) halved; git per tick → tree work only | export back to Markdown (`wsp show --md` for every row) |
| **5. A second instance, failover proven** | Leases for tick, barrier runs, sweeps, delivery; a standby instance on the same machine; the failover test of 8.5 | kill -9 the leader mid-tick 100× across the five danger points: 0 lost or doubled orders, barrier runs or spawns; take-over < lease ttl + one tick | run one instance (the code is the same) |
| **6. Network transport and auth** | TCP+TLS (or WebSocket) `Connection`; principals with credentials; remote executors connect without ssh forwarding | a remote executor runs a row with no ssh tunnel; an unauthenticated call is refused | ssh-forwarded unix socket (today's path) |

Nothing in steps 1–5 builds network transport or sharing (Ed's rule). Step 6 waits until the local server runs the
fleet.

**What the manager steps change in steps 1–6** (wsp-652):
- **Step 1** folds the managers' events into its mirror, rather than mirroring files that probes wrote. It is served
  over the network from the start (D19).
- **Step 2** keeps the orders machine. Delivery and ack already come from the manager (M1), so the step is the store
  owning the order record and the `deliver:<seat>` lease. Its screen-render measure is met at M1.
- **Step 3** shrinks to *the server takes the manager connections over from the daemon, and the seat machine owns
  the record*. Its ps, lsof and cwd deletions are done at M1 and M3. Its herdr rollback is gone with M0.
- **Step 6** keeps auth and principals only. Network transport moved into step 1 (D19), and into M1 for managers,
  which carry one token each until D13 is settled.

### Step 0's breakdown (wsp-043, 2026-11-17)

The list is `s0-state-core`, at status init. Per d1 it runs after M3, so it is not started. It follows wsp-018's
rules. Each row adds something beside the old code and is green on its own, and nothing in wsp uses what step 0
adds, so there is nothing to contract. Each row's checks build the workspace and test every crate it touches, with
budgets of at least 120 s per cargo step (wsp-015).

| group | kind | row | tier | what | creates / changes |
|---|---|---|---|---|---|
| 1 | components | wsp-045 | sonnet high | C compiles through sccache (section 9, option 1), and SQLite enters the tree | creates `.cargo/cc-wrap`, `crates/store-sqlite` (`wsp-store-sqlite`: `rusqlite` bundled, pinned, no type exposed), `tests/cc_wrap.rs`; changes `.cargo/config.toml` (`[env] CC`), `Cargo.toml`, `Cargo.lock`, `tests/layering.rs` (rusqlite in `wsp-store-sqlite` only), docs/components.md (D2's amendment to the one-external-dependency gate) |
| 2 | contract | wsp-046 | opus high | The contract's types (2.1, 2.2, the commands of 4.2, the event semantics of 4.3), types only (R2), by a row of its own (R5) | changes `crates/store-contract` (new modules, `MAP.md`, a serde round-trip test), `Cargo.lock`, docs/components.md |
| 3 | components | wsp-047 | opus high | `wsp-store-conformance`: the ten cases of 2.3 as generic functions, `mem::MemStore` (the fake), a test domain, and bite tests that show each case fails against a fake broken in the matching way. Three legs: `fake`, `state`, `events` | creates `crates/store-conformance`; changes `Cargo.toml`, `Cargo.lock`, `tests/layering.rs`, docs/components.md, this section's 2.3 |
| 4 | insitu | wsp-048 | sonnet high | The measure: a fresh worktree builds `wsp-store-sqlite` in debug and release with sccache warm from another tree, plus both fallbacks, written into section 9 as "Step 0's result" | changes this document (sections 7 and 9) |

The groups run one at a time. Each row of groups 1–3 changes `Cargo.lock`, and groups 1 and 3 each add a workspace
member on `Cargo.toml`'s one `members` line, so in parallel they would conflict at land. The stop after group 3 asks
for sccache to be installed before the measure.

**Found while planning.** sccache is not installed on this Mac: it is not on `PATH`, in `/usr/local/bin` or in
`/opt/homebrew/bin`. So `.cargo/rustc-wrap` falls back to bare rustc today, and no Rust compile is cached across
trees either. Section 9's figures come from Ed's machine. On this one (M4 Pro, 12 cores, load about 4, no cache), a
scratch crate on `rusqlite 0.32.1` with `libsqlite3-sys 0.30.1` bundled built cold in 3.1 s (debug) and 12.7 s
(release). The `libsqlite3-sys` rebuild alone took 2.0 s (debug) and 12.1 s (release). So debug already builds
under 5 s here without a cache. The cache matters for release builds (`wsp install`, `verify --release`) and for the
i5.

**Left for the person** (the review note on wsp-043):
- (a) Where `apply`'s validation runs. Recommended: the contract declares a `Domain` trait with two signatures,
  `decide(&State, &Command)` and `evolve(State, &Event)`. A backend runs `decide` and appends its events in one
  atomic write, which matches 2.2's `apply(cmd)`. The suite brings a test domain now, and `wsp-machines` brings the
  real one.
- (b) Installing sccache on this Mac, before group 4.
- (c) Whether step 0 waits for M3, as d1 says (recommended), or rides beside it (Q14).
- `wsp-api` (4.1's envelope) is not in step 0. It belongs to step 1's breakdown, where whether it reuses
  `wsp-host-api`'s `Connection` is decided.

### Step 1's breakdown (wsp-069, 2026-11-17)

The list is `s1-state-core`, at status init, not started. Per d1 it runs after step 0's list is complete (and so after
M3). It follows wsp-018's rules: expand, then contract; a cut that cannot be staged is one row run as legs; every row's
checks build the workspace and test each crate it touches, with budgets of at least 120 s per cargo step. It also follows
wsp-068's recommendation: rows on one seam never share a group, and each overview names its seam and files.

**What step 1 is, as amended.** The daemon opens `state.db` through `wsp-store-sqlite` and keeps a **mirror** in it. The
mirror is a projection, never the owner of any state, and it is built from two sources:
- the managers' events, applied by src/fold.rs (wsp-028) beside the files it already writes;
- the store's files (rows, claims, lists, asks, machines), re-read by the daemon when `Store::fingerprint` or
  `attention_stamp` moves. The daemon checks these in process once a second, with no child.

Files stay the truth. The API serves the mirror's reads and `subscribe` over TCP on loopback and the tailnet (D19).
web-chrome's census wakes on its events, not on the `wsp stamp` poll, and `wip --json` reads through it. Nothing is deleted
in step 1, because both rollbacks (the host's poll and the CLI's files) must stay one flag away. The contract half comes
with steps 2–4, when the machines own what the mirror only copies.

| group | kind | row | tier | what | creates / changes · seam |
|---|---|---|---|---|---|
| 1 | contract | wsp-070 | opus high | The contract carries the mirror: `Command::Mirror { put, gone }`, `mirror.put`/`mirror.gone`, and `fields` on `Seat`, `List`, `Group`, `Ask`. `TestDomain` refuses `mirror` | crates/store-contract, crates/store-conformance/src/domain.rs, docs/components.md · the contract's types |
| 2 | contract | wsp-071 | opus high | `wsp-api`: 4.1's envelope, the read verbs and `subscribe`, `hello`/`welcome` with `api.token`, `api.json` for where it listens, `Principal`'s text form, its own `Connection` (types only) | creates crates/api; Cargo.toml, Cargo.lock, tests/layering.rs, docs/components.md · the API's wire types |
| 3 | components | wsp-072 | opus high | `SqliteStore` implements `Store` and passes all ten cases of 2.3, plus `open_read_only`. Three legs: `apply`, `leases`, `events` | crates/store-sqlite; Cargo.lock, tests/layering.rs, docs/components.md · the SQLite backend |
| 4 | components | wsp-073 | opus high | `wsp-server`: TCP on loopback and the tailnet, `hello`, the read verbs, `subscribe` with beats, commands `refused unsupported`, and `mirror::Mirror`, the domain step 1 runs | creates crates/server; Cargo.toml, Cargo.lock, tests/layering.rs, docs/components.md · the server's socket and subscribe loop |
| 5 | components | wsp-074 | sonnet high | `wsp-client`: dial from `api.json`, `Silent` never a hang, request by id, and subscribe with reconnect, de-duplication and `Resumed` | creates crates/client; Cargo.toml, Cargo.lock, tests/layering.rs, docs/components.md · the wire's client side |
| 6 | wiring | wsp-075 | opus high | The daemon opens `state.db`, starts the server (`WSP_API=off` is the rollback), applies the mirror per manager event in fold.rs, and re-mirrors the files on a fingerprint move. Doctor reports the API. Three legs: `project`, `serve`, `fold` | creates src/mirror.rs, tests/mirror.rs; src/daemon.rs, src/fold.rs, src/main.rs, Cargo.toml, Cargo.lock, tests/layering.rs · the daemon's start-up and the manager fold |
| 7 | wiring | wsp-076 | opus high | `Wip::from_api`: `wip --json` from one `state`, byte-identical to the files path (a test), no child, files as the fallback within 200 ms, `WSP_READ=files` | src/cmd_agent.rs, Cargo.toml, Cargo.lock, tests/wip_api.rs · wip's read path |
| 7 | wiring | wsp-078 | opus high | **compound-react**: web-chrome's `feed.rs` subscribes through `wsp-client` and rings the census `Gate` per subject (row → tasks, ask → flags, seat/claim/order → agents, list → worklists, machine → machines). `WEB_CHROME_FEED=poll` is the rollback, and a silent API falls back to the poll | spikes/web-chrome/src-tauri/{src/feed.rs, src/main.rs, Cargo.toml, Cargo.lock} · web-chrome's census wake-up |
| 8 | insitu | wsp-077 | sonnet high | The measure, after a day on the installed trunk and app, written here as "Step 1's result" | this section |

**Why the groups are serial.** Groups 2–7 each change Cargo.lock, and groups 2, 4 and 5 each add a member on
Cargo.toml's one `members` line, and groups 1–6 each edit docs/components.md. In parallel they would conflict at land. wsp-073's tests run over `MemStore`, so in
principle it does not need wsp-072. Group 7 is the only shared group. Its two rows are in different repositories, with
no file and no seam in common.

**The stops.** After group 6: wsp-078 must sit in a project whose root is compound-react. After group 7: the person
installs the trunk and web-chrome's app, and a day passes on both before the measure.

**Found while planning.**
- **web-chrome's polling is host-core's `wsp stamp` loop**, not the reads. `push_census` (src-tauri/src/main.rs) runs
  `host_core::poll::spawn`, one `wsp stamp` every 2–5 s. Each stamp that moves rings census.rs's `Gate`, whose reads are
  `wsp` children (`ls --json --all`, the flags read, `wip --json`, the worklists read, `machine ls --json`). So the
  smallest change that takes polling to 0 is to ring the same gate from `subscribe`. The census's reads, parsers and host-core
  stay as they are (d3: Ed expects no compound change; host-core is shared with the gpui window).
- **wsp has no project `compound`** (`wsp project show compound`: no project matching). wsp-078 is filed under `wsp`
  until the person makes one. Its tree is the machine's compound-react checkout.
- **The contract cannot write a projection.** Its 19 commands are 4.2's verbs, each with section 3's guards, so step 1's
  mirror needs one contract change (wsp-070, R5) rather than a backdoor in the SQLite crate. A write path outside the
  contract would make the server depend on `wsp-store-sqlite`, which section 6 forbids.
- **The daemon already folds the managers' events** (src/fold.rs, wsp-028) on a thread of its own. The mirror hangs off
  that fold, so the amendment's "folding the managers' events, not files written by probes" costs no second subscription.

**Left for the person** (the review note on wsp-069, each with a recommended answer):
- (1) The API's transport. Recommended: TCP on loopback and the tailnet with one token per instance, as the manager does
  (D19, agent-manager 3.4). `$WSP_STATE/api.json` names where it listens and stands for 4.1's `api.sock`, with no unix
  socket path.
- (2) Whether `wsp-api` reuses `wsp-host-api`'s `Connection`. Recommended: no. wsp-api declares its own trait over its own
  `Frame`, with the same framing rule copied. host-api's trait is typed to the manager's `Frame`, and making it generic
  changes the manager's contract (R5) for no caller's gain.
- (3) How the mirror is written. Recommended: one contract command, `mirror`, decided by the server's `Mirror` domain and
  removed entity by entity at steps 2–4, plus `fields` maps for what wip's json needs and the typed fields lack.
- (4) Output compatibility (Q5) and the CLI's fallback (Q4). Recommended: `wip --json` byte-compatible through step 1,
  proven by a test, with the files as the fallback while they are the truth. The read-only database fallback of 4.6 (c)
  waits for step 4.
- (5) A wsp project for compound-react (`wsp project add`, rooted at the compound-react checkout), and
  `wsp mv wsp-078` into it. Needed by the stop after group 6.
- (6) Whether web-chrome later folds `State` itself (4.4's `store` half) rather than re-reading on each event.
  Recommended: not in step 1. Re-reading on events already takes polling to 0. Folding means new parsers in compound for
  the contract's entities, which belongs with step 4, when `--json` may change shape.

### Step 1's result (wsp-077, 2026-11-20)

**Verdict: none of the four done-when lines is met, and the rollback is proven for one half of its two flags.** The
API path is built and serving, and the stamp poll did stop. But `wip --json` through the API costs **11× the wall and
20× the CPU of the files path** on this machine, so the step made the call it was meant to make cheap dearer. The
lines are marked below, each with its follow-up row.

**What the measure could not be.** The stop in front of this group asked for a full day on the installed trunk and
on web-chrome's app. The person passed it on 2026-11-20 on judgement, not on the full day, and named the gaps to record
without blocking on them. They hold, and they bound every figure here:
- **There is no pre-group-6 day on this host.** This is the Linux host (32 CPUs). The daemon that wrote the first
  record of `calls.jsonl` (2026-11-19T07:29:08Z) was already serving the API (`api.log`: `started … on 127.0.0.1:7488`
  at 07:29:20Z). So every figure below is an *after*. The before figures are 1.5's (the Mac, before M0) and the Mac's
  own `calls.jsonl`, which is not reachable from here (`wsp machine ls`: this seat only). They are not comparable for
  tick or CPU timings, as the person said.
- **Not a full day on the app.** The person's note says web-chrome polled `wsp stamp` until 2026-11-20T06:30Z because
  the Mac held a stale `api.token`. The Mac's own calls are the ones that carry that, and they are not read here.
  What this host's log holds are the calls that reached it over ssh (parent `sshd-session`), which is where the
  Mac's target-mode reads land. Measured from 06:30Z the window is 20 minutes long at the time of the last read.
- **Windows, all UTC.** The log covers 2026-11-19T07:29:08Z to 2026-11-20T06:50Z (23 h 21 min). The installed binary
  is `ff9afa6`, replaced on disk at 2026-11-19T08:55Z; the daemon running it has run since 11:49:13Z (the person's note:
  it restarted 12:49 CET). The trunk is 29 commits ahead of it (`wsp doctor`), among them wsp-304 and wsp-305.

**How each figure was taken.**
- *Day figures*: `wsp calls --since 24h` (`--polls` for the reads), and a script over the same two files
  (`calls.jsonl`, `polls.jsonl`) for what `wsp calls` does not split: by parent, by hour, children by verb and flag,
  and the `"k":"tick"` lines. CPU is user+sys of the process and its waited children, as the log records it.
- *A/B, `wip --json`*: the installed `wsp` against itself with `WSP_READ=files`, alternating, `WSP_NO_CALLS=1` so the
  fleet's log is not touched, CPU by `wait4` as M0's result took it. Run twice: n=16 at load 9.4, and n=20 at load
  4.8. Same store, same minute, so the two paths read the same fleet.
- *Events per hour*: the `event` table of `state.db`, opened read-only, grouped by hour and subject.
- *Daemon CPU*: `/proc/<pid>/task/*/stat` of the running daemon (pid 2477339, 18.97 h old).
- *Server's share*: a raw client (`hello`, then `state`) timed the reply alone.

#### The measures

| measure | before | after | how |
|---|---|---|---|
| `wsp stamp` calls from the app's host | Mac, 1.5: 2,059 per 4.5 h (458/h, p50 216 ms). This host, 08:00–10:21Z on 11-19: 1,588 (677/h, p50 6 ms), all `sshd-session` | 44 in the whole rest of the log: 42 at 17:03–17:17Z, 2 at 20:58Z. **None since 20:58:54Z (9.9 h). None since 06:30Z.** | `wsp calls --since 24h --polls`; the script, by parent and time |
| the reads that remain (remote host), per hour | 800 calls/h in the stamp era (stamps and reads), 11 CPU-s/h | **366 calls/h, 29 CPU-s/h** over the 20.5 h since; **an idle hour is 298 calls/h** (`machine ls` 119, `throttle` 119, `spend` 60), 2.4 CPU-s/h | the script, `sshd-session`-parented calls |
| events per hour (the mirror's) | | 360/h at idle, all of them `mirror.put` of the machine; 430–1,630/h when the fleet is busy | `state.db`, `event` table |
| `wip --json`, the day | 1.5: p50 1,597 ms, 1.54 CPU-s, 299 calls per 4.5 h | **p50 498 ms, p95 631 ms; CPU p50 0.452 s, p95 0.583 s**, n=2,355. 256 ms at 08Z on 11-19, 600 ms from 02Z on 11-20. Since 06:30Z: p50 695 ms, p95 989 ms, CPU 0.637 s | `wsp calls --polls`, by hour |
| `wip --json` A/B, API against files | | API: wall p50 **693 ms** (p95 978), CPU p50 **0.628 s** (p95 0.928), n=20. Files: wall p50 **61 ms** (p95 72), CPU p50 **0.032 s** (p95 0.039), n=20. The first run, n=16: API 774 ms and 0.702 s, files 65 ms and 0.035 s | `ab.py`, alternating |
| `ps`/`lsof` children of read verbs | | `wip` 0 of 2,937 calls, `wip --json` 0 of 2,355, `show` 0 of 1,883, `ls` 0 of 39, `worklist show` 2 of 3,431. **`ls --json` runs one `ps` in 1,129 of 1,139 calls, with 5.5 `compound-reader`; `worklist show --json` runs `ps` in 216 of 381.** `lsof`: 0 in all. | the script, `children` by verb and flag |
| daemon tick | 1.5 (Mac, before M0): p50 2.2 s, p95 54 s. M0's live day (Mac): 85 ms / 1.60 s | whole log, this host: **p50 20 ms, p95 640 ms** (4,137 ticks at the first read). First 4.33 h (776 ticks): p50 17 ms, p95 381 ms. Last 18.97 h (3,381 ticks): p50 20 ms, p95 664 ms | the `"k":"tick"` lines |
| daemon CPU | none on this host | process 433 s in 18.97 h (**22.8 CPU-s/h**). **Mirror thread 51.6 s (2.7 CPU-s/h, 12%)**; `fold:<host>` 140.7 s (7.4/h); the tick thread's own user+sys 8.1/h | `/proc/<pid>/task` |

Where the 693 ms goes: the server answers `state` in 22 ms, but the reply is **6.84 MB** (5.94 MB of it the 366
task rows with their prose, 0.61 the 74 lists, 0.31 the 132 asks), and the client takes the other ~670 ms and 0.6
CPU-s. The split between decoding it and `Wip::from_api` was not profiled (gdb could not attach). One suspect is
visible in the code: `from_api` runs `mirror::task_of` on **every** row and only then lets `wip_reads_in_full` drop
the finished ones, where the files path does not read those bodies (`tasks_read_where`). The cost also rose
through the day, 256 → 600 ms, as the store grew, which fits a per-call cost in the whole state.

**The outputs are not identical either.** The A/B's two outputs differ by 19.5 KB (42.9 KB against 23.3 KB; the n=16 run
51.7 against 32.1). The API's `agents` has 29 entries and the files path's 14: the API draws **15 `gone` seats with
no date** (`cpd-33`, `cpd-41`, … `cpd-130`, no `task`, `since` set to the read's own clock), which the files path
retires. `GONE_FOR` leaves an undated seat drawn, by design ("every refusal leaves the row drawn"), which suggests the
mirror does not carry these seats' dates; that was not traced. Section 7 asked for byte-identical output through
step 1, shown by a test over a sandbox; that test (`tests/wip_api.rs`) passed when the row landed, and a live fleet
with old seats shows the gap.

#### Against step 1's four done-when lines

- **Host polling calls → 0: not met.** The stamp half holds on this host: 1,588 stamps in the 2 h 21 min before
  10:21Z yesterday, 44 in the 20 h after, 0 for the last 9.9 h and 0 since 06:30Z. It is not shown for the app's own
  host (the Mac's log is out of reach), where the person's note says it polled until 06:30Z. And the figure the row
  names is *reads per hour against events per hour*, which is not quiet: an idle hour still runs 298 reads against 360
  events, because every one of those events is the machine's 10 s beat arriving as a `mirror.put`, and the installed
  app rings the census on it. The reads run on events, but not on events that changed anything. Host polling CPU did
  not fall either: 11 CPU-s/h before the stamps stopped, 29 after, because the census's `wip` (493 CPU-s in the 20.5 h)
  is now the dear call. Both fixes for the beat are on the trunk and not installed: wsp-304 (the daemon sends
  `machine.beat`, not `mirror.put`) and compound-react-046 (web-chrome's `Seen` rings nothing on it). Follow-ups:
  the idle re-measure after installing both, and the Mac's side.
- **`wip --json` < 100 ms and < 0.1 CPU-s: not met.** p50 693 ms and 0.628 CPU-s on the API path, against 61 ms and
  0.032 on the files path in the same A/B. The files path, the rollback, is the one under the line. Follow-up: the
  API read, and separately the 15 undated seats.
- **No `ps` or `lsof` in read verbs: not met.** `wip`, `show` and the plain `ls` and `worklist show` hold (`lsof` is 0
  everywhere). `ls --json` and `worklist show --json`, the census's two reads, run a `ps` per call and, with it, the
  `compound-reader` reads. They are in the brief's four verbs in their `--json` form, and web-chrome's census calls
  them. wsp-308 (wsp-view takes State alone) moves the census's readers into the fold and covers this for web-chrome;
  the CLI's own `--json` read is not covered by it. Follow-up.
- **Daemon tick no slower: not met, because it cannot be shown.** There is no pre-API day on this host. The two halves
  of the log are the same code and differ by what the fleet was doing: p50 17 against 20 ms, p95 381 against 664 ms,
  and the second half contains the busy 21Z–02Z stretch (p95 up to 1.19 s) the first does not. The quiet hours inside
  each half agree (p50 16–18 ms in both; p95 180–384 ms in 08–10Z on 11-19 and 339–372 ms in 15–20Z), so nothing here says
  the tick got slower, and nothing says it did not. The mirror thread's own cost is measured now: 2.7 CPU-s/h, 12% of
  the daemon's 22.8. Follow-up: an A/B on this machine, a day with `WSP_API=off` and a day without, which is the
  rollback flag doing the job of a before.

#### Rollback, tried once each

- **`WSP_READ=files`: works.** `WSP_READ=files wsp wip --json` read the files and answered in 65 ms (run 1) and 61 ms
  (run 2). To show it dials nothing, both paths ran in a network namespace with no network (`unshare -rn`): the default
  path printed `wsp: wip reads the files: nothing answered: 127.0.0.1:7488: connection failed: Network is unreachable`
  and answered from the files (26 ms, five runs), and `WSP_READ=files` printed nothing and answered. Same output, exit 0
  each. The silent API's fallback is therefore also proven, and it is well inside its 200 ms.
- **`WEB_CHROME_FEED=poll`: not tried.** The app runs on the Mac, and nothing here can run it: no app process, no
  display server, and the one build there (`spikes/web-chrome/src-tauri/target/debug`, dated 2026-11-19) is not known to be the
  installed app. The code says it works (feed.rs's `Mode::Poll`
  "runs host-core's poll as before and never dials"), but that is the code, not a call. Follow-up: one call on the
  Mac, below.

#### Follow-ups

Filed as rows in project wsp (not sub-tasks of wsp-077, which is a member and has none), each with a Done-when line. wsp-316 and wsp-317 are the API read, wsp-318 the `--json` reads' children, wsp-319 and wsp-321 wait on the person (the install, the Mac), and wsp-320 is a day on each side of `WSP_API=off`.
- **wsp-316**: `Wip::from_api` and the `state` reply: p50 < 100 ms and < 0.1 CPU-s in this A/B at n ≥ 15, output
  unchanged. Until it lands, `WSP_READ=files` is the faster default by 11× and 20×, and the person may want it set at
  install.
- **wsp-317**: the API's `wip --json` draws 15 undated `gone` seats the files path retires; the mirror
  carries the seat's date so both paths agree on a live fleet, shown by a test with an old seat.
- **wsp-318**: `ls --json` and `worklist show --json` run no `ps` and no `compound-reader` in the CLI's own read.
- **wsp-319**: install the trunk (wsp-304) and the app (compound-react-046), then
  re-measure an idle hour: reads per hour against `machine.beat`-free events per hour.
- **wsp-320**: the tick's before, by a day with `WSP_API=off` against a day with it, same machine; and the
  daemon's CPU-s/h on both.
- **wsp-321**: on the Mac, the stamp count from its own `calls.jsonl` since 06:30Z, and `WEB_CHROME_FEED=poll` tried
  once (the app launched with it, one stamp seen in `wsp calls --polls`, then without it, none).

### Step 2's breakdown (wsp-115, 2026-11-18)

There are two lists, both at status init and not started. `s2-state-core` runs after `s1-state-core` (d1). It ends at a
step only the person can do, which is installing the trunk with the store as the default (wsp-091). `s2-state-core-2`
waits on it. It follows wsp-018's rules: expand, then contract; a cut that cannot be staged is one row run as legs; each
row's checks build the workspace and test every crate the row touches, with budgets of at least 120 s per cargo step. It
also follows wsp-068's rule (one seam, one group) and wsp-086's (no row here loads the machine on purpose, so none needs
a group of its own for that). Each overview names its seam and its files.

**What step 2 is, as amended.** Orders become commands to the store, and section 3.1's machine owns the order record
and the `deliver:<seat>` lease. The manager already delivers and acks (M1: `order.deliver` → compound-sup's `deliver`
verb, and `order.delivered`/`order.acked`/`order.refused`), and the daemon already folds those answers (src/fold.rs into
`manager/<host>.orders.json`, and mirrored as `Order`s since wsp-075). What is missing is the owner. Today an order has
no record of its own before the manager answers. Its id is minted by the seat's directory (`deliver.pending` in
place_compound.rs). Three files and a phase stand in for its state:
- `typed-unsent.json`: `repair::unsent_tick`, the `repair:unsent` phase. It already skips itself under the manager.
- `first-orders.json`: `owe_first_order` from five sites in cmd_spawn.rs, and `first_order_tick`'s press, resend and
  flag.
- `stuck-commands.json`: `stuck_tick`'s record of a stalled command and whether the seat was told.

A fourth, `refused-deliveries.json` (refusals.rs, wsp-634), counts refusals toward ending a deaf governor.
untaken.rs (wsp-252) presses and resends from cycle.log markers. These files and that pressing are what the machine
replaces.

**Found while reading the code.**
- **Every send has one choke point**: `agent_commands::Kind::tell` → `Place::tell` → `Compound::tell` → `deliver_through`
  → `Host::deliver` (crates/wire/src/host_tcp.rs: `order.deliver` to the manager under wsp's id). The senders above it
  are `wsp tell` (cmd_agent.rs `tell_seat`), answers (cmd_message.rs `deliver`, `deliver_held`/`type_at`), wake.rs
  (`deliver_to` and its read at :1116), cycle.rs `tell_seat`, the spawn (cmd_spawn.rs `start_ordered`/`confirm_turn`),
  repair.rs `stuck_tick`, and untaken.rs. Each moves over in one row of group 5.
- **The first order already carries a wsp-minted id** (`FirstOrder { order: place::new_order_id(seat) }`, cmd_spawn.rs),
  so the store's order id can replace it with no wire change. A told order's id is minted in the wire crate
  (`delivery_id`), so the wire needs `deliver_order` with a caller's id (wsp-120). That is the new row of that number,
  not D20's herdr row.
- **The manager answers an id at most once**, and it keeps an unanswered order in `<exec_dir>/orders/` across a restart
  (crates/manager/src/seats.rs). So the delivery worker can lose its lease mid-delivery and the next holder asks again
  under the same id without doubling. Fencing protects the store's record, and the manager's key protects the seat.
- **stuck-commands.json is not delivery.** It records a stalled shell command (wsp-397). Under the manager the stall is
  already the folded `seat.stuck` (`manager/<host>.stuck.json`), and the file adds only `told`. That becomes the state
  of the order that told the seat (part 4 below).
- **The contract has `order.send` and `order.cancel` and nothing that moves an order on**, and the server refuses every
  command (`unsupported`, crates/server/src/conn.rs). There is no `wsp-machines` crate yet. So the expand half is
  contract → machine → server → the daemon's worker, each beside the old path.
- **The daemon and the server share one process** (wsp-075), and the daemon holds the manager connections until step 3.
  So delivery runs in the daemon, through the in-process store, and not in the server (part 1 below).

**List `s2-state-core`: expand, move over, default on.** Every root row is behind `WSP_ORDERS` (`files` is today's path
and the default until group 7).

| group | kind | row | tier | what | creates / changes · seam |
|---|---|---|---|---|---|
| 1 | contract | wsp-117 | opus high | The contract carries the machine (R5): `order.deliver` (fenced on `deliver:<seat>`), `order.delivered`, `order.acked`, `order.refused { why, counts }`, `order.done`; `first` on `order.send` and `Order`; `LeaseName::deliver` | crates/store-contract, crates/store-conformance/src/domain.rs, crates/server/src/mirror.rs, docs/components.md · the contract's types |
| 2 | components | wsp-118 | opus high | `wsp-machines`: `orders::Orders`, a pure `Domain` with 3.1's table, named refusals, the attempts cap, and repeat answers as empty decisions; case 9 against it | creates crates/machines; Cargo.toml, Cargo.lock, tests/layering.rs, docs/components.md · the orders machine |
| 2 | components | wsp-119 | sonnet high | `wsp-client`: `apply` under one key across retries, `await_order` over subscribe | crates/client · the wire's client side |
| 2 | components | wsp-120 | sonnet high | `Place::deliver_order(seat, order, text)`: tell's supervisor path under the caller's id, with no `deliver.pending` | crates/wire/src/{place.rs, place_compound.rs, fake.rs} · place_compound's deliver |
| 3 | components | wsp-121 | opus high | `wsp-server` composes `Orders` and `Mirror`. Under `Owned` it accepts `order.send`/`order.cancel` from clients and the answers only from the instance or a manager. The daemon opens it `Mirrored` (no change) | crates/server, Cargo.lock, tests/layering.rs, docs/components.md, the root's open call site · the server's command path |
| 4 | wiring | wsp-122 | opus high | src/orders.rs, the door (`mode`, `send`, `answer`, in process or through the client), and src/deliver.rs, the worker: `deliver:<seat>` taken, `order.deliver` fenced, `deliver_order` under the order's id, the answer applied. Two legs: `door`, `worker` | creates src/orders.rs, src/deliver.rs, tests/orders.rs; src/daemon.rs, src/main.rs · the daemon's start-up and delivery |
| 5 | wiring | wsp-123 | opus high | The fold applies the manager's order answers as machine commands under the worker's keys, and `order.done` when the acked turn ends | src/fold.rs · the manager fold |
| 5 | wiring | wsp-124 | opus high | `wsp tell` and answers: `order.send`, then the ack, with the receipts and `--json` unchanged. A held answer is a queued order | src/cmd_agent.rs (`tell_seat`), src/cmd_message.rs, tests/tell_orders.rs · the CLI's tells |
| 5 | wiring | wsp-125 | opus high | The daemon's tells: the governor's spool and its confirmation, barrier checks, the stall's telling, and the deaf governor read from `failed` orders | src/wake.rs, src/cycle.rs (`tell_seat`), src/repair.rs (`stuck_tick_with`, `refusal_tick`), src/refusals.rs · the daemon's tells |
| 5 | wiring | wsp-126 | opus high | The spawn's first order is `order.send { first }`, carried in `seat.spawn` under its id. untaken waits on `failed` and does not press | src/cmd_spawn.rs, src/untaken.rs · the spawn's first order |
| 6 | wiring | wsp-127 | opus high | wip's `unsent`, `dropped`, `refusing` and `stuck`, and waiting.rs's `unsent`, read from the orders and the folded stall, with `--json` byte-identical | src/cmd_agent.rs (`Wip`), src/waiting.rs · wip's seat columns |
| 7 | wiring | wsp-128 | opus high | `WSP_ORDERS` defaults to `store` under the manager, and doctor names the mode. `files` is the rollback | src/orders.rs, src/daemon.rs · the orders flag |

**List `s2-state-core-2`: measure, then delete.** It runs after `s2-state-core`, once the person has installed it.

| group | kind | row | tier | what | creates / changes · seam |
|---|---|---|---|---|---|
| 1 | insitu | wsp-129 | sonnet high | The measure, held until 14 days after the install: typed-but-unsent incidents and wait-loop stalls over the fortnight, renders per tick, `wsp calls` a day before and after, written here as "Step 2's result" | this section |
| 2 | removal | wsp-130 | opus high | Delete `typed-unsent.json`, `first-orders.json`, `stuck-commands.json`, `refused-deliveries.json`, `repair:unsent`, `deliver.pending`/`delivery_id`, the `files` branches, `WSP_ORDERS` and `Orders::Mirrored`. Three legs: `root`, `wire`, `server` | src/ (repair, refusals and every order path), crates/wire/src/place_compound.rs, crates/server · every order path |

**Why the groups are what they are.** Groups 1–4 are serial because each builds on the one before (types, machine,
server, worker), and groups 1–3 each edit docs/components.md. Group 2 is the only components group run side by side.
Its three rows name disjoint files, and only wsp-118 touches Cargo.toml, Cargo.lock or tests/layering.rs. Group 5's four
rows are on four seams with disjoint files: fold.rs; cmd_agent.rs's `tell_seat` region with cmd_message.rs;
wake/cycle/repair/refusals; and cmd_spawn/untaken. All four read src/orders.rs and none changes it. Their tests live in
their own files (wsp-124's in tests/tell_orders.rs, not wsp-122's tests/orders.rs). wsp-127 is a group after wsp-124
because both edit src/cmd_agent.rs, in different regions. wsp-128 flips the default only after every reader has moved, so
the trunk never reads a file the default stopped writing. wsp-130 is alone because it touches every order path.

**The stops.** After `s2-state-core`'s group 7, the person installs the trunk (`wsp install`) and restarts the daemon.
That ends the list. `s2-state-core-2` starts once it is done, and its group 1 is held `--until` 14 days after the
install. After its group 1, the deletion goes ahead only if the measure reads met. Otherwise the list holds and the
`files` path stays the rollback.

**Left for the person** (one `wsp ask` on wsp-115, each part with a recommended answer):
- (1) Where delivery runs, meaning who holds `deliver:<seat>`. Recommended: a thread in the daemon process beside the
  fold, through the in-process store. It is woken by `wait_for`, not paced by the tick. Step 3 moves it into the server
  together with the manager connections.
- (2) The orders machine under `WSP_HOST=inproc`. Recommended: `store` only under the manager. The in-process host keeps
  today's path until M1's contraction removes it, and wsp-130 waits for that.
- (3) Attempts and a busy seat. Recommended: the worker delivers only to a seat the mirror reads `idle` (or `started`,
  for a first order). A `busy` refusal is no attempt (`counts: false`). The cap is 3, then `failed`, which wip draws as
  today's `order not received` or `refusing`, and which ends a deaf governor (wsp-634).
- (4) stuck-commands.json, which records stalls, not deliveries. Recommended: the record is the folded `seat.stuck`
  (the seat machine's at step 3), and the telling is an order keyed by (seat, stall start), whose state replaces `told`.
- (5) refused-deliveries.json, which section 3.5 does not name. Recommended: retire it with the others in wsp-130. The
  deaf-governor ending reads the seat's `failed` orders.
- (6) When the `files` path is deleted. Recommended: after the fortnight's measure passes (list 2: measure, then
  delete), so the rollback stays one flag away (`WSP_ORDERS=files`) through the measure. After the deletion the
  rollback is a revert. This replaces section 7's "repair:unsent restored behind a flag".
- (7) `wsp tell` from a person's shell. Recommended: unchanged as the person sees it. It sends `order.send`, waits on the
  order's events for today's 30 s, and prints the same receipt and `--json`. The tick never waits. It queues and reads
  the order's state on the next tick.
- (8) Two lists, split at the install (wsp-091). Recommended: yes, as filed.

---

## 8. Resilience and scale

### 8.1 What runs on every instance, and what is leased

| work | where | lease |
|---|---|---|
| reads, `subscribe`, accepting connections | every instance | none |
| commands (`apply`) | on SQLite: forwarded to the writer (D1: one writer with a lease); on Postgres: any instance | `writer` (SQLite only) |
| the tick's timed work (start groups, expire claims, notice lost executors) | one instance | `tick`, ttl ~3 ticks |
| a barrier run | one instance or worker | `barrier:<list>/<n>`, key = (list, group, attempt) |
| each sweep (scratch, build output, check trees) | one instance per machine (sweeps act on a machine's disk) | `sweep:<kind>:<machine>` |
| order delivery to a seat | one instance | `deliver:<seat>` |
| executor connections | any instance; the executor reconnects elsewhere | the executor's own lease (5.1) |

**State lives only in the store.** An instance's memory holds caches it can rebuild from `snapshot()` +
`events_after`. A work item that must run once is a lease plus an idempotency key on its effects. A new holder of a
lease that died mid-work replays the same key, and the effect happens once (a spawn keyed on (row, attempt), a
delivery on order id, a barrier on (list, group, attempt)).

### 8.2 Leader leases and fencing

A lease is a row: `(name, holder, token, expires)`. `take` is a compare-and-set: free or expired, then
`token += 1`. A holder renews at ttl/3. Every write made because of the lease carries `Fence { name, token }`, and
the store refuses it if the token is stale (2.2). Clocks: on one machine the server's clock decides. Across
machines the ttl must exceed the worst clock skew plus one renewal. Tailscale hosts with NTP run well under 1 s
apart, but that is still to be measured. Expiry by the store's own clock (Postgres `now()`) is preferred to
expiry by the client's.

### 8.3 SQLite first: one writer with a lease, and standbys

**Same machine (step 5).** Several instances open one database file in WAL mode. Readers run concurrently, and
SQLite serialises writers by its own lock. D1 still asks for a **writer lease**: one instance applies commands,
and the others forward commands to it over the socket. That keeps the "only the leaseholder writes" rule identical
on SQLite and Postgres, and keeps the subscribe wake-up in-process. The alternative is that every instance writes
and SQLite's lock serialises them. That is simpler and correct on one host, but it is a different model from
Postgres. That choice is Q10.

**Across machines (a standby on a second host).** SQLite cannot be shared over a network filesystem safely, so a
standby needs replication:

| option | how | gives | costs |
|---|---|---|---|
| **Litestream** | streams WAL frames to object storage or another disk; a standby restores on promotion | point-in-time backup; RPO ≈ 1 s; no live replica (restore is the failover) | small: a sidecar process; S3/R2 at cents per month for this size (the store is ~30 MB); failover = restore time (seconds for this size, to be measured) |
| **LiteFS** | FUSE filesystem, primary via a lease (Consul or static), live read replicas | live replicas, fast failover | FUSE on macOS (macFUSE, a kernel extension) is the blocker; Fly.io has de-prioritised LiteFS; more moving parts than the problem |
| **wsp's own** | standbys follow `events_after` over the API and keep a copy | uses the event log already built; replicas are readable | the replica's write path must be refused until promotion; a bespoke replication to maintain |

**Recommended:** Litestream as the backup from step 1 (cheap, and it is a backup whatever else is chosen). Do not
build cross-machine standbys on SQLite at all. When a second host needs to run an instance, that is the trigger for
Postgres (8.4).

### 8.4 Postgres later, and what active-active needs

**What Postgres gives.** Many writers. `SELECT … FOR UPDATE SKIP LOCKED` for claiming work. Advisory locks or the
same lease rows for leaders. `LISTEN/NOTIFY` as the wake-up (the event table stays the truth, and NOTIFY is only a
nudge).

**What is hard: events in commit order.** A `bigserial` id is assigned when the insert runs, not when the
transaction commits. A reader can therefore see id 101 committed while 100 is still in flight, and then miss 100
when it reads `from 101`. The options are:
- (a) Serialise event appends on one counter row, `UPDATE seq SET n = n + 1 RETURNING n`. This is simple and caps
  write concurrency at one appender, which is ample for one tenant.
- (b) Read only up to the oldest in-flight transaction (`pg_snapshot_xmin`), as outbox relays do.
- (c) Logical replication or LSN-ordered reads.

**Recommended (a).** Write rates here are tens per minute. Case 5 of the conformance suite is what proves whichever
option is chosen.

**The contract that keeps the backend swappable.** This is section 2.2: domain operations only, no SQL, connection
or row type crossing, and one suite (2.3) that every backend passes. The SQLite backend's SQL is SQLite's own
(D1).

**The trigger for the move.** It is any one of:
- a second machine must run a server instance (cross-host high availability);
- more than one tenant must share a database server for operational reasons (instancing does not require this);
- the sustained write rate passes what one SQLite writer handles with headroom. The estimate is about 1,000
  commands/s against today's well under 1/s, so this is not the binding trigger.

The likely trigger is the first one. Proposed numbers are Q11.

**The cost of the move.** It is a new `wsp-store-postgres` crate the size of the SQLite backend, estimated at
1.5–3k lines with the schema, plus a Postgres in CI for the conformance suite and a one-off export/import. Nothing
above the contract changes. If anything does, the contract leaked, and that is a bug in the SQLite step that must
be fixed there.

### 8.5 Failover, measured

The test is a sandbox (`wsp sandbox --fake`) with two instances and fake executors. A harness `kill -9`s the leader
at each of five points, 20 times each:
1. after `order.typed` is written and before the executor is told;
2. after the executor said `delivered` and before it is recorded;
3. mid barrier run;
4. between `seat.requested` and the executor's `claim`;
5. mid sweep.

After each kill the harness waits for take-over and checks, from the event table alone:
- every order reaches `acknowledged` exactly once;
- every barrier key has one verdict;
- every (row, attempt) has at most one `seat.started`;
- no fenced write from the dead leader is visible after the new token.

It records take-over time, expected to be at most the lease ttl plus one tick. This is step 5's exit measure.

### 8.6 Instancing per tenant

A tenant is a full instance set: its own state directory (`WSP_STATE`, as sandboxes already are,
crates/store/src/store.rs:2154), its own database file, its own socket (`$WSP_STATE/api.sock`), its own executors
(an executor's `hello` names its tenant, and a token for one tenant is refused by another), and its own principal
namespace. No row, event or lease carries a tenant column, because isolation is the instance. Running two tenants on
one machine means two server processes and two launchd labels. Today's sandbox code shows that works.

---

## 9. SQLite build caching, measured

Measured 2026-11-08 on this machine. Four scratch crates (in `$WSP_SCRATCH/sqlite-cache`, standing in for four
worktrees) each depend only on `libsqlite3-sys =0.30.1` with `bundled`. That is the C part `rusqlite` pulls in. Each
uses wsp's own `.cargo/rustc-wrap` (sccache for rustc). The builds were `cargo build --release --offline` under
`nice`.

| tree | how C is compiled | wall | user CPU | sccache |
|---|---|---|---|---|
| a (first) | `cc` directly (today's wrapper) | 34.0 s | 27.6 s | Rust: 1 hit, 5 misses |
| b (second tree, same sources) | `cc` directly | **30.0 s** | **27.6 s** | Rust: 6 hits; **the C compile is not cached** |
| c (first, C via sccache) | `CC="sccache cc"` | 31.8 s | 1.7 s (the compile ran in sccache's server) | C: 1 miss |
| d (second tree) | `CC="sccache cc"` | **3.4 s** | 1.6 s | C: 1 hit, Rust: 6 hits |

**What this shows.** Today's wrapper caches the Rust crates across worktrees, but not `sqlite3.c`. cc-rs only
wraps C compiles with sccache when `RUSTC_WRAPPER` is literally sccache, and ours is the script
`.cargo/rustc-wrap`. Every new tree would pay about 28 CPU-s to recompile SQLite, unless it is warmed (`warm.rs`
copies `target/`, which carries the built `OUT_DIR`). With C routed through sccache, a second tree builds it in
3.4 s.

**Recommended for step 0.** Route C through sccache for the whole repository. Two ways:
1. `[env] CC = "…/cc-wrap"` in `.cargo/config.toml`, a twin of `rustc-wrap` that calls `sccache cc` and falls back to
   `cc` when sccache is missing or its server is down (wsp-343's rule).
2. Set `CC` in the throttle's environment.

Option 1 also benefits compound, which shares the wrapper (compound-965). Only the release profile was measured
here. The debug profile compiles SQLite at `-O0` and will be cheaper, but it should be measured at step 0.

The alternative is no bundling: link macOS's system `libsqlite3`. That costs no C build at all, but the SQLite
version is the OS's, which matters for `RETURNING` (3.35+) and JSON functions, and it differs on a far machine.
Bundled plus cache is the safer default. This is Q12.

### Step 0's result (wsp-048, 2026-11-18)

**Method.** Both profiles were warmed in the row's own tree with `cargo build -p wsp-store-sqlite` (debug, then
`--release`), so sccache held `sqlite3.c`'s object for each. The cold tree is a fresh `git worktree add` of the trunk
(`2b2150c`) under `$WSP_SCRATCH` with an empty `target/` before every run. It is not `warm.rs`'s copy, so there is no
`OUT_DIR`. Each run is `nice cargo build -p wsp-store-sqlite --timings` (plus `--release`), as section 9 did. Three
runs per figure, p50 shown, the three in brackets. M4 Pro, 12 cores, macOS 27.2.1, rustc 1.100.0, sccache 0.19.0,
`rusqlite` bundled with `libsqlite3-sys`. The build-script figure is the `libsqlite3-sys` `run-custom-build` unit as
`--timings` reports it. The wall is the whole command, 49 units. Load is the 1-minute average at the start of the run.
The fallback configs ran on an isolated sccache server (`SCCACHE_SERVER_PORT`, `SCCACHE_DIR`), so no other tree's
server was stopped.

| config | profile | `libsqlite3-sys` build script, p50 (runs) | whole command, wall p50 (runs) | load | C compile through sccache |
|---|---|---|---|---|---|
| **cached** (cc-wrap, sccache warm from another tree) | debug | **1.18 s** (1.04, 1.18, 1.75) | **7.7 s** (5.7, 7.7, 7.7) | 7.7–10.6 | hit in every run |
| | release | **1.11 s** (0.38, 1.11, 1.11) | **6.1 s** (4.9, 6.1, 9.7) | 12.0–14.7 | hit in every run |
| fallback: `COMPOUND_NO_SCCACHE=1` | debug | 2.47 s (2.22, 2.47, 3.51) | 9.7 s (9.2, 9.7, 9.9) | 16.9–20.8 | bypassed |
| | release | 13.83 s (13.82, 13.83, 13.94) | 22.3 s (21.4, 22.3, 24.3) | 21.3–23.1 | bypassed |
| fallback: sccache's server stopped (it starts a fresh, empty one) | debug | 3.49 s (2.47, 3.49, 3.53) | 12.9 s (11.4, 12.9, 14.0) | 21.6–23.1 | miss, 0 hits |
| | release | 14.58 s (13.89, 14.58, 15.17) | 26.0 s (23.9, 26.0, 26.5) | 23.2–26.3 | miss, 0 hits |
| fallback: server dead (port held by a listener that accepts and closes; sccache exits 2) | debug | 4.71 s (3.13, 4.71, 8.03) | 63.8 s (48.2, 63.8, 80.2) | 41.5–66.6 | exit 2, bare `cc` and `rustc` |
| | release | 15.13 s (15.05, 15.13, 21.76) | 33.8 s (28.6, 33.8, 36.5) | 55.9–66.4 | exit 2, bare `cc` and `rustc` |
| *before: wsp-043's scratch crate, no sccache* | debug | 2.0 s | 3.1 s | about 4 | none installed |
| | release | 12.1 s | 12.7 s | about 4 | none installed |
| *before: 2026-11-08, Ed's machine, `libsqlite3-sys` alone, release* | release | 27.6 CPU-s | 30.0–34.0 s (tree b with `cc`) vs 3.4 s (tree d, `sccache cc`) | not recorded | tree d: hit |

Every config built (exit 0 in all 24 runs). All figures are in `$WSP_SCRATCH/logs` (`results*.jsonl`, one
`--timings` html per run); they are not kept in the tree.

**The < 5 s line, judged.**
- *SQLite's own build, from cache: met, both profiles.* The build script takes 1.2 s in debug and 1.1 s in release,
  against 2.5 s and 13.8 s with the cache bypassed in the same session. For release that is 12× less, and it is
  inside the 3.4 s that section 9 measured on Ed's machine. The cached runs show a C hit and no C miss in every
  release run and in 2 of 3 debug runs. The debug run that shows one C miss ran on sccache's shared server beside
  other trees' builds, so the miss may be theirs; this run's build script took 1.18 s, which is a hit's time here (a
  miss takes 2.5–3.5 s).
- *The whole `cargo build -p wsp-store-sqlite` wall: not met.* It is 7.7 s in debug and 6.1 s in release at p50, with
  one release run under 5 s (4.9 s). The build script is about 1 s of that. The rest is the other 48 units:
  `serde_derive`, `zerocopy`, `hashbrown`, `wsp-store-contract` and `wsp-store-sqlite` itself, each a Rust compile
  that sccache serves as a hit but that still passes through `rustc-wrap`, a client round trip and, for registry
  crates with an `OUT_DIR`, a content hash of it. It is not SQLite, and a cache miss on the C compile does not
  explain it. The first run of each cached profile also missed on Rust (7 debug, 14 release), the workspace crates
  that this tree built at another path; later runs hit.
- *Why the wall is not a clean comparison.* The runs were taken at load 8–15 (cached) and up to 67 (the dead-server
  runs), where the before figures were taken at about 4. Other panes were building on this Mac, and the average
  reached 99 afterwards, with `yes` loops among the busiest processes. The cache's effect shows in the build-script column, which does not
  depend on how many other crates are queued. The wall does: debug's cached 7.7 s sits under the no-cache 9.7 s, but
  the load was 8 against 17. The before figure of 3.1 s was also a scratch crate with only `rusqlite` and its
  dependencies, not the 49 units `wsp-store-sqlite` brings in. So debug was never the case the cache helps (it
  already built in under 5 s without one at load 4), and release is: 22.3 s without the cache and 6.1 s with it
  (3.7×), at loads of 21 and 12.
- Nothing was tuned to make a figure pass. A rerun at a quiet load would settle the wall; the machine did not
  get quiet during this row.

**The fallbacks.** Both build, and cc-wrap's rule (wsp-343) holds where it can be tested.
- `COMPOUND_NO_SCCACHE=1` builds with the bare `cc` and `rustc`: 2.5 s and 13.8 s for the build script, in line with wsp-043's
  12.1 s release figure for the same compile at a lower load.
- A stopped server is not a failure for the client: `sccache cc` starts a new one. The row shows that case, a fresh
  empty cache, so the C compile misses and costs what it costs without a cache (3.5 s, 14.6 s), plus a few seconds
  of cold Rust misses.
- A dead server, one that refuses or drops the connection, makes sccache exit 2, and both `cc-wrap` and `rustc-wrap`
  fall back to the bare compiler. The build script is 4.7 s and 15.1 s. The walls (63.8 s and 33.8 s) were taken at
  load 42–67 and say only that the build finishes; every rustc call pays a failed sccache call first.
- **A hung server is not covered.** The first attempt at the dead-server case held the port with a listener that
  accepted connections and never answered. The first debug run finished (exit 0) after 591 s, and the second sat for
  over five minutes in `sccache … rustc-remap -vV` before it was stopped by hand. The fallback waits for sccache's
  exit 2, and a server that is alive but not answering does not give one quickly. This was seen on one run at load
  26, and the cause was not isolated. It is a risk for wsp-343's rule, not a change made by this row: an `RUSTC_WRAPPER`
  that bounds the client's wait (a timeout around the sccache call) is the usual shape of a fix, and nothing here
  adds it.

---

## 10. Decisions for Ed

1. **The target, corrected.** Do you accept the four corrections in the short answer: tree work stays client-side,
   machine facts come from executors, git is the larger probe, and Postgres event ordering needs designing?
2. **`~/wsp/tasks`.** Option A, B or C (2.5)? The recommendation is C: prose stays in Markdown under git, and
   everything machine-written moves into the store.
3. **Order of steps.** Is step 1 (read path plus compound subscribing) first, ahead of orders? It retires the
   largest single waste (15% of CPU), but it mirrors files rather than owning state, so it is a projection to delete
   at step 4.
4. **Server down.** Option (b) kick-and-retry with (c) read-only fallback for reads, or refuse everything (a) (4.6)?
5. **Output compatibility.** Should `--json` output stay byte-compatible until step 4 (agents and compound
   unchanged), or only until compound subscribes (step 1)?
6. **Identity.** Is per-seat tokens handed out by the executor, plus the peer uid on the socket, enough for day
   one, with a person principal `person:ed`?
7. **Event retention.** Keep every event forever (today's 9 MB `events.jsonl` suggests years fit), or compact past N
   months to snapshots, with `subscribe from` an old id answering `too_old`?
8. **Executor shape.** One compound-sup per machine as the executor, or keep one compound-sup per seat behind a
   per-machine executor face? *(D14: Ed's lean is "one central process that spawns durable supervisors". Carried
   forward as docs/agent-manager.md Q1–Q3, which recommends one agent manager per host over one compound-sup per
   seat, ahead of the server.)*
9. **herdr seats.** Should herdr panes be a second executor kind behind the protocol, or be retired once compound
   seats run everything? *(Settled by D15: there is no herdr executor. Its removal, wsp-120, is step M0 of
   docs/agent-manager.md, which is agent-manager Q9.)*
10. **One host, several instances.** One writer with forwarding, as D1 reads, or let every same-host instance write
    under SQLite's own lock and keep the writer lease only for once-only work (8.3)?
11. **The Postgres trigger.** Is "a second host must run an instance" the trigger? Should a write-rate or tenant
    count be named too (8.4)?
12. **SQLite build.** Bundled with C through sccache (recommended), or the system `libsqlite3` (9)?
13. **wsp-474 as a guard.** Should the contract-change share become a standing check on `wsp-store-contract` and
    `wsp-api`, with a share above ~35% slowing the migration (6)?
14. **When step 0 starts.** Does step 0 (the wrapper, the contract types, the suite against a fake) ride the tail of
    fleet-fixes, or wait for the pass to finish as D4 says for execution?
