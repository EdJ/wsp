# Hand-off: the rest of wsp-read-api (poll-call load and the flaky suite)

From Ed's side, 2026-11-20. This hands Jesper's side the work that was left on Ed's `wsp-read-api` list. Re-plan it on your end, file it in your own store, and run it there. Ed's side has cut the list at group 2 and will not run any of it.

**Why it's being handed over:** to avoid conflicts. Every row here touches the API, server, client, mirror or test-harness code that Jesper's side is also changing (wsp-030 and the turnaround hand-off in `docs/turnaround-handoff.md`). Running it on one side avoids two trees rewriting the same files.

**The ids are Ed's.** Both stores use the `wsp-` prefix and number independently, so `wsp-688` here is not your `wsp-688`. Each row's whole content is copied below, so you don't need Ed's store. File them under your own ids.

## 1. What has already landed (on master with this branch)

Groups 1 and 2 of `wsp-read-api` passed and are on master:

| Commit(s) | Ed's row | What it did |
|---|---|---|
| 00a8cc2 | wsp-692 | The API client finds a frame's end in linear time (crates/client/src/dial.rs). It measured `state` at 51.4 MB, past the 16 MiB `MAX_LINE`. |
| 6b26cb5 | wsp-693 | The fold scripted-stream and repair governor-reseat tests wait on what they observe. |
| 9bfa6ec | wsp-698 | wip_api's load check accepts the thermal warning sentence, and the two joined. |
| f89bdba | wsp-700 | cc_wrap's hung-sccache tests no longer depend on when the fakes get scheduled. |
| 17079a4 … 95d4f71 | wsp-695 | A new `wip` read verb, cut in SQL over the state row, about 12–13 MB on the live store (under `MAX_LINE` 16,777,216). The mirror writes `head` on every row. Every list that isn't finished keeps its body, and so does a finished list that a check or governor still sits on. |
| — | wsp-697 | Survey only, no code: it found every whole-state reply over `MAX_LINE` and filed rows 702–704 (section 4). |

`wip` still reads the files by default (`da64352`). The API path only runs under `WSP_READ=api`, until the wsp-696 work below flips it.

## 2. The poll-call load (was group 4): the point of the list

web-chrome's census polls `wip --json`, `ls --json --all` and `worklist show --all --json` every few seconds. On 2026-11-19 those three together kept about one core busy. Ed's side won't open the Compound UI until they're cheap. All three share one target: **under 100 ms wall and under 0.1 CPU-s** (the bar in `docs/state-core.md` step 1), measured before and after at a stated load average below 4. Each must be byte-identical to its files path (tested the way `tests/wip_api.rs` tests wip), start no child process, and fall back to the files within 200 ms on a silent or failed dial. `WSP_READ=files` forces the files path.

**Never read `state` or the subscribe snapshot on the real store.** Both are about 51 MB, past `MAX_LINE`. They get no reply and the caller falls back silently.

Start from your own `docs/state-core.md` step 1 result (6ee2f50): "the API wip is 11x slower than the files path". Measure against those numbers.

### 2a. wip --json through the API meets step 1's bar and becomes the default (Ed's wsp-696, high)

Builds on wsp-695's `wip` read. Point `from_api` (src/cmd_agent.rs) at that read and remove its remaining file reads. One is known: `cycle::silent`'s `find_task` falls back to a whole `tasks()` walk for an archived member. Then measure, and flip `wip_reads_api` so the API is the default again.

It also owns two things wsp-695 left behind:
- **Narrowing the files path:** `wip_tasks` (src/cmd_agent.rs, about line 4541) should read in full only open rows. wsp-695's live-store diff showed wip and wip --json unchanged with that narrowing (235,297 B both ways, only timing noise). Leave Here's use of `wip_reads_in_full` alone.
- **A test on a seeded store** that wip's output from the `wip` reply equals the files path.

**Overlap:** the flaky-test fix in 3a may change `throttle_of_facts` in src/cmd_agent.rs. Build on whatever it leaves there rather than reverting it.

**Done when:** on this store `WSP_READ=api wsp wip --json` runs under 100 ms wall and 0.1 CPU-s, measured at a stated load below 4; `tests/wip_api.rs` still shows the two paths byte-identical; and in the same change, with `WSP_READ` unset, wip reads the API.

**Ed's planned legs:**
1. count-reads (sonnet medium): list every file read left in `from_api`.
2. point-and-flip (opus high): remove them, measure, flip the default.

### 2b. `ls --json --all` reads through the API (Ed's wsp-688)

`wsp ls --json --all` parses every task file on each call. Measured 2026-11-20 at load about 20: 1.59 s wall, 1.0 CPU-s, 3,500 store files and 42 MB in tasks/. The output is 3.2 MB: 3,116 rows of head fields only (after, caption, created, id, parent, priority, project, refs, stage, status, tags, title, updated), with no bodies. web-chrome's census asks for it through host-core's `Kind::Tasks` every 5 s.

`rows {}` as it stands returns whole rows with bodies, about 40 MB, so read heads only. In src/mirror.rs every field ls prints except `stage` is already a scalar in `Row.fields`: status, prio, tags, caption, project, parent, after, refs, created, updated. The body sits under the BODY or PROSE key, and since wsp-695 every row also has a `head`. Whether `stage` can be had without the body is the one open question.

Reuse wsp-695's server and client plumbing rather than adding a verb when a flag on `rows` will do, for example a `RowFilter` field that leaves BODY and PROSE out (crates/store-contract/src/store.rs, crates/store-sqlite/src/reads.rs). Build on wsp-view's shapes for `ls --json`. Don't write a second shaping path.

**Ed's planned legs:**
1. explore (sonnet medium): settle `stage`, and measure a heads-only `rows` reply.
2. build (opus high): the read crosses the contract, store, server and client crates.

### 2c. `worklist show --all --json` reads through the API (Ed's wsp-689)

It parses every list and its rows on each call. Measured 2026-11-20 at load about 20: 2.04 s wall, 1.97 CPU-s. The output is 2.9 MB: 82 lists, each with at, barrier, behind, dangling, dropped, groups, hold_caption, overview, unrun, unwritten, waiting_on and worklist. The census asks for it through `Kind::Worklists` every 15 s. In the old web-chrome app's last 10 minutes it was the heaviest call: 104 CPU-s over 28 calls.

The read it needs is the lists with their overview (the list's prose, which the `wip` read leaves out) plus their member rows' head fields (status, title, caption, after), never row bodies. Settle first whether `lists {}`'s contract Lists serve this as they stand, and measure that reply on the live store.

**Ed's planned legs:** explore (sonnet medium), then build (opus high). Same reasons as 2b.

## 3. The flaky full suite (was group 3)

Group 2's check found three groups of tests that keep the full suite failing on a loaded machine, or inside a check seat. Until they're fixed, every group check that runs the whole suite fails for reasons unrelated to the change. All three were found at 95d4f71. None had started when the list was cut: the trees were clean, with no commits.

### 3a. wip_api's api-vs-files test fails when the CPU clock is thermally limited (Ed's wsp-705)

`tests/wip_api.rs:368` (`wip_reads_the_same_through_the_api_and_the_files`, throttle `on`) failed 3 of 3 runs of `cargo nextest run -p wsp --test wip_api` beside 8 busy loops (hw.ncpu 8). It passes alone in 8 s. The only difference between the two paths:

- api: `throttle: on — in force · check slots 0/4 · building: …`
- files: `throttle: on — in force · cpu clock <n>% · check slots 0/4 · building: …`

The files path reads pmset's CPU_Speed_Limit and draws the cell (crates/trees/src/throttle.rs `thermal_cell`). The API path's `throttle_of_facts` (src/cmd_agent.rs) draws from the beat's `speed_limit_pct` (crates/host/src/facts.rs `assemble`) and never shows it. Either the beat's pmset read is missing or timing out under load, or the loopback manager in the test doesn't read it. If the beat really drops the clock, that's a product bug: `wip` under the API hides a thermally limited clock. Fix it there, not by masking the cell.

**Done when:** that test passes 5 runs in a row beside about core-count busy loops, with the cpu clock cell unmasked (or only its number masked, as `idle_masked` already does), and the note says which side was wrong. This blocks 2a's byte-identical test.

### 3b. wsp-manager's seats tests miss their 5 s spawn answer in parallel under load (Ed's wsp-706)

A `--no-fail-fast` workspace run failed four tests in crates/manager/tests/seats.rs, each after about 5.1 s:
- `two_spawns_with_one_key_give_one_seat` (seats.rs:303, "both spawns are answered seat.started")
- `an_order_answers_delivered_then_acked`
- `a_stop_or_an_order_naming_a_stale_epoch_is_refused`
- `a_turn_with_no_order_reads_working_while_open_and_idle_at_its_stop`

`cargo nextest run -p wsp-manager -E 'binary(seats)'` alone failed a different three, and with `-j1` all 7 pass. So the tests' WAIT, or the fake sup's start, is too short when the 7 tests start sessions at the same time.

**Done when:** that command passes 10 runs in a row at default parallelism beside about core-count busy loops. The waits must hang off what the test observes (the session's start), not a longer timer alone. Say what was racing.

### 3c. 19 closing/cmd_worklist/cycle tests fail inside a check seat (Ed's wsp-707)

With a check seat's environment set (WSP_SEAT_KIND=check, WSP_EXIT, WSP_VERDICT, WSP_SEAT_ID, WSP_SEAT_EPOCH), 19 tests in the wsp binary fail:
- `closing::tests::a_check_*` (3)
- 14 `cmd_worklist::tests` hold, barrier and park tests
- `cycle::tests::a_held_runs_member_reopened…` and `a_member_reopened_under_a_hold…`

All pass with those five unset. The fix is to clear the seat environment under the tests' isolation (SEAT_ENV), not to unset it by hand.

**Done when:** `cargo nextest run -p wsp --bin wsp -E 'test(/^(closing|cmd_worklist|cycle)::tests::/)'` passes with those five set to a check seat's values, and a test pins that the isolation clears them.

## 4. Related: other replies past MAX_LINE (filed by wsp-697, never planned to a group)

These go with the read work, so they're yours too if you want them.

- **Ed's wsp-702 (high):** `subscribe {}`'s opening snapshot is 51,520,142 B. The client reconnects every 25 ms forever, costing the server about 1 s and 49 MB of encoding per round. Fix: send the snapshot as several frames, each under `MAX_LINE`, and back off after a loss that delivered nothing. Done when it delivers on the full store (state the largest frame's size), and a test pins the backoff.
- **Ed's wsp-703 (after the `wip` read):** `state` (51.5 MB) and `rows {}` (40.6 MB) are sent anyway and the client drops the connection with `ConnError::Io`. Fix: refuse an oversized reply with a named code, and page `rows` (or offer heads only, which 2b may already give). Done when the full store answers with the code, and paged `rows` returns every row with each page under `MAX_LINE`.
- **Ed's wsp-704:** the `events {from: 1000, limit: 1000}` page is 19.4 MB. Fix: cap a page by bytes as well as `MAX_EVENTS`, and say where it stopped. Done when paging from 0 returns every event, each page under the limit, with none lost or repeated.

## 5. What Ed's side keeps

- **incident-11-19:** the leg-cap stop reaches the governor (wsp-690); seat clean-up is automatic and a broad `pkill` is refused (wsp-691); and a held backlog can't wedge a seat (wsp-694). wsp-691 includes the clean-up gap found today: when an exit is accepted, the pane binding is dropped but the row's claim is not. So closing the seat leaves the row claimed by a seat that no longer exists, and the run won't seat the next leg until someone runs `wsp release <row>`.
- **Finished seats left open under the manager.** Before this merge, `ending::left` read `hooks.jsonl`, which seats run by wsp-manager never wrote. So every finished seat read "cannot say whether anybody has typed" and stayed open, taking a slot under the cap. wsp-030's folded `turns.json` (9ffd75e) should fix this once installed. Ed's side will check it on live seats and pick up anything left over.
