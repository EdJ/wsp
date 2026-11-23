# What the daemon's CPU is spent on (wsp-741)

Measured 2026-11-22/23 on the live store (3211 tasks, 134 panes, ~500 open rows),
daemon pid 77734 up 21 h. Read this before profiling the daemon again: the
method is quicker than it looks.

## How to see it

- **`calls.jsonl`, `"k":"tick"` lines** — one per daemon pass, with `phases`
  (wall ms), `phase_cpu_ms`, and `children` (what each spawned). This already
  names the phase; no profiler is needed to find the pass. Aggregate the last
  few hundred.
- **`ps -M -p <daemon>`** — cumulative CPU per thread. The tick thread (the first
  row) holds ~60% of the daemon's total; `mirror` and `fold` ~15% each.
- **`sample <pid> 5`** on the release binary shows `???` only: it is built with
  `strip = true`. To read a stack, build a copy with
  `CARGO_PROFILE_RELEASE_STRIP=false CARGO_PROFILE_RELEASE_DEBUG=1` into a scratch
  `CARGO_TARGET_DIR` and sample a *verb* that takes the same path, rather than
  the daemon (a second daemon on the live store takes it over).

## What it said

Alternate ticks are light (~0.6 s: tunnels, mute-seats sweep) and heavy (27–58 s).
Over 120 ticks (71 minutes) the daemon's own CPU was 1330 s, of which:

| phase | CPU s | share |
|---|---|---|
| `ending:verification` | 672 | 51% |
| `pickup` | 185 | 14% |
| `attention` | 96 | 7% |
| `sweep:auto-close` | 95 | 7% |
| `cycle` | 73 | 5% |

plus 33 000 `git` children (541 s wall) under `cycle`/`pickup`.

## The cause of the first row

`ending::released_seats` looks every pane's label up with `Store::find_task`. A
label that is no task's id (a governor, the primary seat, a shell) is a *miss*,
and a miss fell through `resolve_task` to `self.tasks()`, which **clones all
~3200 parsed tasks** to read their ids and titles: 90 ms measured in-process,
steady store, warm cache. About 190 misses a heavy tick ≈ 17 s, which is the
phase's figure.

Fixed two ways: `resolve_task` reads `tasks_shared()` (an `Rc`, no copy; a miss
is now under 1 ms), and `released_seats` finds a pane's row with `task_now`,
which guesses nothing — the label is an id wsp wrote — with `renamed_ids` read
once for the census.

## And in `wsp worklist ls --json` / `wip --json`

Both are one-shot, so none of the daemon's caching helps them; they pay every
read cold. For `worklist ls --json`, 86% was `waits::read_on`:

- `room::Records::weight_of` ran `open_list_of` per call, which clones every
  list's groups (and hashes every list body) for each verified row and held
  seat — rows × lists clones, twice a run (`Machine::now` and `idle_on_ed`).
  Now the member → weight map is built once per `Records`.
- `Store::messages()` parsed and rebuilt the 2 MB `messages.json` once per list
  (`phase_of` → `message::raised`). Now `messages_shared()` memoises the built
  map on the file's stamp, and `raised` clones only what needs attention.
- `Store::renamed_ids()` re-read `ids.json` per member (`worklist::member`); now
  memoised on its stamp.

## The figure and what holds it

`scripts/bench-reads.sh` times `wsp worklist ls --json` and `wsp wip --json` on
the live store (they read it and write nothing) and fails over 1 s each. Run it
at idle: under load (the machine read 540 on 2026-11-23) wall time says nothing,
so it also prints the `user+sys` CPU, which is the figure to compare. (It is a
script and not a test because the room count probes the seats through
`Store::open`, which a test may not do.)
`room::tests::a_rows_weight_is_the_first_open_lists_first_groups` holds the
weight lookup's rules (first open list, first group, a draft is nobody's).
