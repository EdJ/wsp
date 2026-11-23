# wsp-743 explore: what the web-chrome census takes, and what the API serves

Measured 2026-11-23 on the live store, read-only (`sqlite3 -readonly ~/.local/state/wsp/state.db`).
MAX_LINE = 16,777,216 (16 MiB). The `state` JSON is 54,886,414 B.

## Reply sizes today (state row's JSON, whole-entity sizes)

| read | bytes | of MAX_LINE | note |
|---|---|---|---|
| `state` | 54,886,414 | 327% | over (wsp-692) |
| `rows {}` | 43,593,764 (3,214 rows) | 260% | over: 2,401 rows carry `body`, 813 `prose` |
| `lists` | 8,062,557 (202 lists) | 48% | fits; 182 finished, 10 running, 10 planned; `body` is most of it |
| `seats` | 36,528 (22 seats) | 0.2% | |
| claims / machines | 429 / 957 | ~0 | |
| asks | 3,176,799 (1,284 asks; 307 raised = 539 KB) | 19% | |
| `wip` (wip_snapshot, summed) | about 14.7 MB (rows 12.19 MB + lists 1.95 MB + raised asks 0.54 MB + seats/claims) | about 88% | fits, but 12% headroom; open rows are whole (review 218 rows = 6.6 MB, todo 624 = 2.5 MB) |

Head-only rows (no `body`, no `prose`): 3,123,338 B for 3,214 rows (19% of MAX_LINE). Dropping the `head`
key too (every census field is already a scalar in `Row.fields`; `head` repeats them plus `sections`):
1,677,971 B, avg 522 B a row. `ls --json --all` output was 3.2 MB (wsp-688).
Lists without `body`/`prose`: 1,726,383 B.

## Per census read

| census read | CLI command | wsp-view fn | inputs |
|---|---|---|---|
| tasks | `ls --json --all` | `task::ls_row(row, archived, stage)` + `task::ls` | row heads; `archived`; `stage` |
| flags | `flags --json` | none (host-core `Kind::Flags`, already the API's asks) | asks of kind Flag, in `State.asks` |
| agents | `wip --json` | `wip::wip_json(&[Agent], &Head)` | see wip below |
| worklists / lists | `list ls --json`, `worklist ls --json` | `worklist::list_json(&[Listing])` | see lists below |
| machines | `machine ls --json` | `machine::list_json(&Fleet)` | see machines below |
| throttle | `throttle --json` | `throttle::json(&Throttle)` (+ `memory_json`) | see throttle below |
| spend | `spend --json` | `spend::report(lines, places, landed, now)` | see spend below |
| wip (attention) | `wip --json` | as agents | as agents |

### tasks (`ls_row`)
- Row head fields (status, prio, tags, caption, caption_from, project, parent, after, refs, created, updated, claim):
  served today as scalars in `Row.fields`, but `rows` returns whole rows. **Needs a head-only `rows`.**
- `archived`: not served. A row not in the live set is archived; the mirror holds all 3,214 rows, so check which
  field says filed away (the files path reads `archived_hits`). **Needs a mirror field.**
- `stage` (`Stage{now: Told, steps: [Step]}`): not served, no `stage` in `src/mirror.rs`. `stage::with_history` derives
  it from the row's log, its verification passes (`verification::all` over every task), the lists' landed members, `said.json`
  and a live agent census for `doing` rows. **Needs a mirror field**, written where the mirror already has the tasks
  (a JSON text of `now` and `steps`, read back by one `stage_of(row)`).

### wip / agents (`wip_json`)
`wip::Agent` has 30 fields; `Head` 10. All come from `src/cmd_agent.rs` (`wip_rows`, `wip_attention`, `wip_queues`,
`answers_to`), not from the contract. What the API serves: seats with `fields` (`binding`, `spawned`, `census`, `agent`,
`label`, `cwd`, `session`) and the claims and open rows. Not served, computed in src/:
`role` (from `governors`, lists' running groups, `verification::seated`, `legs::seated`, `pieces::seated`, checks),
`answers_to` and `answers_to_empty` (`governors::seat_for`/`seat_above`), `waiting` and `wait` (`waiting::reading`,
`seat_wait`, asks and shells), `needs_you`, `turning`, `stuck`, `since` (`seat_clocks`, a store note), `held`, `direction`
(mandates), `queued`, `unsent`, `dropped`, `full_suite`, `leg`, `members_waiting`, `was_governor`, `closed_row`, and Head's
`attention` (classify), `daemon`, `disk`, `load`, `throttle` lines, `idle_memory`, `blocked`, `review`, `inbox`.
`Wip::from_api` already rebuilds all of it from `wip_snapshot` plus a live host census (`host_tcp::census`), in src/.
**This is the one read whose inputs are not a few fields**: the census would need `Wip::from_api`'s result, which lives
in the binary crate and cannot be linked by web-chrome. See "Open question" below.

### lists (`worklist::Listing` and `Head`)
`List.fields` today: `title`, `caption`, `created`, `prio`, `status`, `planned`, `after`, `project`, `alone`, `body`,
`seated`; groups as `Group{state, members, attempt, fields}`. `Listing` needs, in addition: `caption_from`,
`status` phase word and `held`, `waiting_on_ed`, `parked` flags (`worklist::Phase`), `stored` (raw status word), `groups`
count and `at` (can be read from `Group.state`), `outputs` and `outputs_dir` (`cmd_report::declared`, files on disk),
`segment`, `open`, `gone`, `unwritten`, `activity` (`worklist::listing`), `queue{n, waits}` (`waits::read`, whole list queue).
**The body is what the CLI parses for these; the mirror writes none of them as fields.**

### machines (`machine::Fleet`)
Served in `Machine{name, transport, beat, facts}`: facts `os arch status agents lease exec_dir reachable ready tick_secs
last_seen error`, transport `ssh` as `addr`, `beat`. Missing from `Record`: `backend_at`, `sup_at`, `added`;
from `Live`: `leases`, `tunnel`, `socket`, `wire`, `wire_oldest`, `heard`, `checked`, `check_error`, `shim_warning`.
`Fleet.agents` (`Standing`): derived from seats and the binding's task title (`cmd_machine::standing`), a join over
rows. `seat` is the hostname, `seat_cap` is `store.seat_cap()`, `seat_working` is `room::Gate::working()` (reads the
backends). **Needs more `facts` keys, and a served `Fleet` tail (seat, seat_cap, seat_working).**

### throttle (`throttle::Throttle`)
All plain readings from `View::read`: mode, in_force, idle, cores, slots, jobs, load, load_warning, memory
(`memory_json`: free %, swap, growth, pageouts, hold text), `cpu_speed_limit`, thermal flags, hot slots and jobs,
`suggested_agents`, `agents` (`seat_cap`), `builds`. Nothing is in the contract. The throttle crate
(`crates/trees/src/throttle.rs`) computes them from `ioreg`, `pmset`, loadavg, `vm_stat`, the settings file and
the build slots. **Needs a served reading on the local machine** (a fact the daemon writes each tick).

### spend (`spend::report`)
`lines`: `spent.jsonl` ledger (`place_super::SPENT_FILE`), 30-day window, not in the contract. `places`
(seat -> list, project): from `events.jsonl`'s `governor-set`, the lists' groups, passes' and attempts' panes over rows
updated in the window. `landed`: rows with a `Holds` pass decided in the window. `now`: the caller's clock.
Seats that spent are mostly gone, so the 22 live seats do not hold them. **Needs a served read of lines + places + landed.**

## Verdict on the layering

Everything but `wip` is a handful of fields. `wip` is the whole `Wip` gathering. Options for it:
(a) the mirror writes a per-seat JSON text of the finished `wip::Agent` input (and one `Head` record on the local
machine) each pass; the census reads seats and feeds them to `wip_json`; (b) move `Wip::from_api` into a lib crate.
(a) keeps to "serve inputs, no derivation in the client or server". It is the largest line in the next leg.

## Additions for the next leg (contract-store)

Contract (`wsp-store-contract`, `wsp-api`), each a `#[serde(default)]` field or a new read verb with no new meaning
for the old ones:
1. `RowFilter.heads: bool` (or `RowFilter.fields: Fields::Head`): leave out `body`, `prose` and `head`. The 2,286 done
   rows alone are 3.1 MB with them, so it is 1.68 MB for the whole store.
2. `ListFilter` for `lists` (a struct with `heads: bool`; `lists` takes `NoArgs` now): lists without `body`/`prose`.
3. A served-readings read for what has no entity: the throttle `Throttle` record, the spend inputs (lines, places,
   landed) and `seat_working`. One verb (e.g. `readings`) answering a map of name -> JSON text, mirrored from the
   daemon, rather than one verb a reading.
4. `Seat.fields`: `role` (JSON, as `Role::json`), plus `wip` (the finished `Agent` record) per option (a) above.

Mirror (`src/mirror.rs`, write side):
5. Row fields: `stage` (JSON `{now, steps}`) and `archived`.
6. List fields: `caption_from`, the three phase flags, `stored`, `groups`, `at`, `segment`, `open`, `gone`,
   `unwritten`, `activity`, `queue_n`, `queue_waits`, `outputs` (JSON).
7. Machine facts: `backend_at`, `sup_at`, `added`, `leases`, `tunnel`, `socket`, `wire`, `wire_oldest`, `heard`,
   `checked`, `check_error`, `shim_warning`.

Store-sqlite (`reads.rs`): `rows`/`lists` cut in SQL with `json_remove` as `wip_snapshot` does (the present `rows` decodes
the whole state, so a heads read must not); the new filter must not move `state`, `rows`, `lists`, `wip` replies.
Server: handlers over the trait. Client: typed methods, as `wip` got in adfccb5.

## Size of the new replies (estimate, to be re-measured in wire-and-measure)
Head rows 1.68 MB; lists heads 1.73 MB plus about 202 x ~0.6 KB of new fields (~0.12 MB); seats ~37 KB plus role/wip
(~22 x ~1.5 KB); machines ~1 KB plus 12 facts each. All under 12% of MAX_LINE. The existing `wip` at about 88% is the
reply nearest the limit and should be watched, but it is not changed here.

## Open question for the governor
`wip --json` (agents and attention) shapes from a gathering that is src/-only (`Wip::from_api`). Option (a) puts
a written `Agent` record on each seat; the mirror's daemon pass then runs `wip_rows` (it already does, for attention
items, each pass). Confirm (a) before contract-store, as it decides the largest change.

## What contract-store did (880b465 and the commit after it)

Option (a) taken, and widened: d1 says "each becomes a field the mirror writes or a served value". So each
census read's src-only inputs became one **reading** each: the JSON of the wsp-view input struct, which the
daemon takes with the CLI's own builder. They are not fields on entities, because:
- seats are the fold's, and the mirror never writes one, so `role` rides in the `wip` reading's `Agent.role`;
- `worklist ls` and `ls` order and select in src (`worklist::listing`, `keep`'s sort), and a field per entity loses that;
- stages, the throttle and wip move on things no fingerprint covers (said.json, the census, load), so a mirror field would go stale.

So `state`, `rows {}`, `lists {}` and `wip` are byte for byte what they were; no entity gained a field.

Correction to the table above: `ls --json --all` is `Task::json` + `stage` + `legs` (when it has legs; `cmd_task::rows_json`),
not `ls_row`. `ls_row` and `archived` belong to `find --json`. `ls` lists live tasks only, and the archive is not read.

| read | API call | wsp-view | reading (`wsp_view::reading`) | CLI builder it shares |
|---|---|---|---|---|
| tasks | `rows {heads: true}` + `readings {names:["tasks"]}` | `task::ls_json_row(task_json(row), stages[id], legs[id])` over `order` | `Tasks{order, stages, legs}` | `cmd_task::ls_reading` / `tasks_reading` |
| flags | `asks {kinds:[flag]}` | none | none | |
| agents, wip | `readings ["wip"]` | `wip::census_json` | `wip::Census{agents, head, waiting_at_cap, planned_started}` | `cmd_agent::census_of` / `wip_reading` (files path) |
| worklists, lists | `readings ["worklists"]` | `worklist::list_json` | `[Listing]` | `cmd_worklist::listings` |
| machines | `readings ["machines"]` | `machine::list_json` | `machine::Fleet` | `cmd_machine::view(&Fleet::live)` |
| throttle | `readings ["throttle"]` | `throttle::json` | `throttle::Throttle` | `throttle::view` |
| spend | `readings ["spend"]` | `spend::report(lines, &places(), landed, now)` | `Spend{lines, places, landed}` | `spend::reading` |

Contract: `RowFilter.heads`, `ListFilter{heads}` (null or absent `args` still read), `Reads::lists`, and
`Readings{readings, set_reading}` in the `Store` family (default impls: read empty, write refused). `Reading{json, at}`
lives in a `reading` table beside the state: no event, never in `snapshot()`. API: `lists` takes the filter, and
`readings {names}` is answered by `Answer::Readings`. The server's two arms are in. Client methods and the
equality tests against the CLI are wire-and-measure's.
Writer: `src/readings.rs`, a daemon thread beside the mirror. Throttle, machines, wip and worklists are taken every
10 s, tasks every 30 s, spend every 60 s. `WSP_READINGS=off` turns it off.
CLI `--json` sizes measured read-only on the live store (pretty; the readings are compact): wip 299,616; worklist ls
194,848; machine ls 1,124; throttle 546; spend 47,072; ls --all 3,343,254. All well under MAX_LINE.

Governor, 2026-11-23: wsp-view taking `serde` is accepted. It still reads no file, socket, env or clock, and
`tests/layering.rs`'s IO check holds unchanged. wire-and-measure does not reinstall the live daemon. It measures
each reply by building it in-process over a read-only open of the live store, as wsp-695 did; for the readings that
is `crate::readings::take(&store, name)` for each name in `wsp_view::reading::ALL`, plus their sum for one
`readings` reply. It states that the live API serves the readings only after the land's reinstall.
compounding-851 waits on that.
