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
  payload   TEXT NOT NULL                     -- JSON
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
- `subscribe {}` (N absent) sends `{ "state": <snapshot>, "at": M }` first, then events from M.
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
| **M4. Trees placed** (only if Ed chooses T2, agent-manager Q6) | checkout and land on the agents' hosts, with a fenced push to the trunk | a list on two hosts with no copies | T1 (rows on the Mac) |
| **0** (prep, could ride fleet-fixes' tail) | `rusqlite` behind the C-cached wrapper (section 9); the contract crate's types; the conformance suite against an in-memory fake | a cold tree builds SQLite in < 5 s from cache | nothing to roll back |
| **1. Server and store, read path; compound subscribes** | The daemon opens SQLite and **mirrors** today's files into it each tick (a projection; files still the truth). `api.sock` serves `state`/reads/`subscribe`. compound's host subscribes; `wip --json` reads through the API | host polling calls → 0; `wip --json` < 100 ms wall, < 0.1 CPU-s; no `ps`/`lsof` in read verbs; daemon tick no slower | the host goes back to `poll` (one flag); the CLI reads files |
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
