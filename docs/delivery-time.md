# Cutting delivery time

`wsp-377`, 2026-11-05. A plan, not code. Delivery time is the wall from a row
being started to it landing, verified, and its group's barrier passed.

Sources: pre-prep's effort report (`~/.compound/artefacts/pre-prep/effort.json`,
`wsp-375`), `wsp timeline` on its twelve measured rows, `cycle.log` and
`events.jsonl` for 2026-11-05, and the three cases from this week
(compounding-102, compound-934, compounding-061). Figures are seconds unless
they say minutes. Where a figure is read off two logs and not measured, it says
"inferred".

## 1. Where the wall goes

Five pre-prep groups ran start to pass in 348 min. Each group's wall is three
stretches in order, and the wall of a group is **not** the sum of its members
(they overlap), so the report's member-summed figures are read below only for
what they name.

| group | start → last verdict | barrier run | check seat | total |
|---|---|---|---|---|
| 1 | 39.0 min | 7.5 | 3.2 | 49.6 |
| 2 | 70.6 | 20.1 | 4.8 | 95.5 |
| 3 | 84.9 | 5.7 | 4.4 | 95.1 |
| 4 | 36.6 | 7.3 | 2.3 | 46.2 |
| 5 | 29.5 | 15.8 | 16.5 | 61.8 |
| sum | 260.6 (75%) | 56.4 (16%) | 31.2 (9%) | 348.2 |

"Start → last verdict" is the members' stretch: spawn, the seat's work, the
row's own `wsp check`, the verifier, and any block-and-rework round. The barrier
run is wsp's own process (`worklist advance --barrier-run`) over the project's
suite and each member's checks. The check seat is the agent that reads the
packet and passes the barrier.

What each stretch is made of, and who controls it:

| part | measured | wsp controls | wsp does not |
|---|---|---|---|
| spawn → seat claimed | 32–115 s a row, median ~55 | the target clone (333 s once, §3.5), the pane start | claude's own start (seat up after is 2–5 s) |
| the seat's work | member Bash 12 044 s of 12 050 s of tool time; model 2 054 s over 12 rows | what the seat is told to run (`wsp-389`) | the model's time, the project's tests |
| review → verifier spawn | 80–509 s, up to three checks on one review | one check per tip, reuse (§3.3) | the check's own commands |
| verifier reads | 54–914 s, typically 110–180 | reuse of a green (`wsp-389`) | the reading |
| block → rework → re-review | 11.2, 11.7, 14.5 min (3 of 11 rows) | how fast the finding reaches the seat (1–2 min now) | the fix itself |
| barrier run | 342–1197 s a group | duplicates, serial steps, no per-step figure; the wait itself is hidden by `wsp-414` (parallel groups, bigger lists; `wsp-362`, `wsp-363`), not shortened here | the suite |
| run end → check seat started | 2–9 s, but 9.4 min in group 5 | the clone and the retry (§3.5) | |
| check seat | 136–290 s, 7 min in group 5 | auto-pass, reading verdicts it was given | the stop's judgement |
| hand-offs between legs | 5–10 s (compounding-061) | nothing worth cutting | |
| daemon tick (20 s) | no gap in the twelve rows is labelled a tick | nothing worth cutting | |

The barrier run's 56 min is a wait the plan does not try to shorten. Ed's note
on this plan (2026-11-05): the barrier's own latency is hidden by parallel groups
and bigger lists (`wsp-414`, picking up `wsp-362` and `wsp-363`), not cut by
overlapping verifiers or by a faster run. Where this document prices the barrier
run (§3.8, §4) it says what `wsp-414` hides; the reuse overhead, auto-pass, the
cap and the label are priced as below.

The group after one starts before the previous check has passed in 3 of 4
transitions (group 2 at 14:16 against a pass at 14:17, group 3, group 5), so the
check seat is mostly **off** the wall of the next group's start and on the
wall of the row's landing "and its barrier passed". Group 3 → 4 is the
exception: 14.5 min between the pass (17:24) and wsp-382's start (17:39), not
explained here (§3.9).

## 2. The 2.2 h of unnamed waits

The report's first wait is 7 848 s "nothing the logs name". It is two things,
and both are wsp's own, named.

**6 070 s (77%) is the barrier run.** `wsp timeline` measures the gap from the
last verdict to the barrier's `barrier run` line in `cycle.log`, but that line
is written **when the run ends**, with its elapsed time (`barrier run — 9
step(s), 0 reused, 9 ran, 0 failed, 1197s` at 15:46:43 is the run that began at
15:26:46, right after the last verdict at 15:26:39). So the "gap" is the run,
labelled as nothing. The effort report adds the gap once for **each member** of
the group, so a group's run is counted 1–4 times:

| group | run | members | counted |
|---|---|---|---|
| 1 | 450 | 2 | 900 |
| 2 | 1 204 | 2 | 2 408 |
| 3 | 344 | 4 | 1 376 |
| 4 | 438 | 1 | 438 |
| 5 | 948 | 1 (wsp-386; wsp-385's 948 is filed under the cap, below) | 948 |

Distinct, that is 3 384 s, 56 min, which is the barrier column above. The same
mislabel is compounding-102's "160 s the logs name nothing for": a one-step
barrier run of 155 s that failed (`workspace-port group 6`, 11:49:08).

**1 778 s (23%) is the row's own `wsp check`**, run by `wsp review` before the
verifier is spawned. The `task-checked` events end within seconds of each
verifier spawn (wsp-376: check ends 13:50:50, took 142 s, verifier spawned
13:50:55). The gap is 109–780 s, longer than a tick, so it is called unnamed.
On wsp-379 the check ran three times (368, 370 and 208 s, the first two
overlapping) for one review; on wsp-360, wsp-354, wsp-382 and wsp-385 it ran
twice.

Cause (`wsp-392`): each check started at the tick's ready commit, and the cycle
rebases a ready member onto the trunk before the gate, so a trunk that moved
during a six-minute check gave the next tick a new tip and a second check beside
the first. Now the rebase waits for a check under way, and a check that passed
at the pre-rebase tip is carried to the rebased one (a `carried` entry under
## Verification) instead of being run again.

The other three waits:

- **The seat's cap, 3 454 s**: 2 506 s of it is the cap, and the fix
  `wsp-385` (landed) already ends the seat that held a place after its barrier
  passed. Barrier 2 passed at 15:51:33 and its check seat cpd-2999 held one of
  the four places until at least 17:35 (told to close at 17:29 after 5 min idle),
  1 h 44 min of a quarter of the cap, while group 3's rows waited on it. A cap
  gap also holds a check: `wait_name` tests the cap first, and any cap line in
  the window relabels the whole gap, including a row check that was running in
  it (wsp-360's 1 307 s holds a 509 s check). The other 948 s is **wsp-385's
  barrier run** mislabelled: the test is `line.contains(row id)`, and the id
  appears in a cap line as a *holder* of somebody else's wait.
- **The row's own seat finishing, 1 217 s**: not wsp (wsp-385's seat took 20 min
  to type `wsp review`).
- **The hand-off, 6 s**: a poke and a spawn. Not worth a row.

**The overhead of a fully reused run (146–452 s with nothing rerun)** is not
overhead of reuse. It is the wait for the tree lock behind another copy of the
same run. `cycle.log` for pre-prep group 4 says "its barrier run ended without a
result (pid 13446), starting it again (try 2 of 3)" at 18:16:34, 42 s after the
first start, and again at 18:17:06: three copies of one run. They share one
warm tree behind one lock (`barrier/<project>-<repo>/tree`, `TREE_WAIT` 45
min), so the copies sit in `TreeLock::take` until the first ends, and the line
reports elapsed time including that wait. Pre-prep group 4: the first ended
18:23:09 (436 s), the second 18:23:52 (437 s, 3 reused, 0 ran), a reused run's
own cost about 43 s. Remote-agents group 5 shows the same shape: three lines at
18:16:03, :04 and :32 with 439, 146 and 452 s. Inferred from end times: the real
cost of a reused run is 1–43 s, and the copies cost no wall but a process, the
lock, and a figure that reads as slow.

**Auto-pass.** `would pass without an agent (auto-pass is off)` is on all three
remote-agents group 5 lines, so a green run with a mechanical stop would have
spent no check seat there (the check is 136–290 s, and 7 min in group 5). None
of pre-prep's groups would have passed: their stops are prose and their members
needed a judgement. Auto-pass is therefore priced as zero until the groups carry
stop-checks (§3.7).

## 3. Candidates, priced

Saving is minutes on the critical path of a pre-prep-sized group (average of
the five unless it says otherwise); effort is S (a day or less of a seat), M, L.
The measure is a figure in the *next pass's* `effort.json` that says it worked.

| # | candidate | saving per group | effort | the measure |
|---|---|---|---|---|
| 3.1 | name what ran (`wsp-390`) | 0 itself; unlocks the rest | S | unnamed wait seconds under 5% of gap seconds (was 63%: 7 848 of 12 525) |
| 3.2 | one barrier run per tip (`wsp-391`) | 0 wall; 2 processes and a lock a group | S | one `barrier run` line per group (was 3 on two groups) and no run line longer than its steps |
| 3.3 | one check per review tip (`wsp-392`) | 2–6 min on the rows that ran it twice or three times (about 1 620 s over five rows) | S–M | `task-checked` events per review at one; review → verifier spawn at the check's own `took` |
| 3.4 | fewer full-suite runs in members (`wsp-389`, filed) | the largest: member Bash is 12 044 s; compound-934 spent 21.8 of 44 min | M | median member Bash seconds (400–2 549 now), and the verifier read near its 110–180 s typical (wsp-375's 914 s was a rerun) |
| 3.5 | fast check start (`wsp-393`) | 1.9 min average (9.4 min once: 333 s clone, 21 min of "starting", a 3 min back-off), ~30 s on each spawn if the claim time is the clone | S–M | run end → check started ≤ 20 s; spawn → claimed under 20 s median (55 now) |
| 3.6 | cap held by finished seats (`wsp-385` landed; `wsp-394`) | 4–13 min on group 3 alone (wsp-360 waited ~800 s, wsp-334 ~490 s) | S, to show it | cap seconds per group, with the holder named and whether it had finished; no finished seat in a holder list |
| 3.7 | auto-pass with stop-checks (`wsp-396`) | check seat 2.3–4.8 min a group, plus a place and a governor turn; 0 until stops are mechanical | S to write, Ed's call to turn on | check-seat seconds in the report; groups passed by wsp |
| 3.8 | overlap barrier steps (`wsp-395`) | none on the delivery wall by itself: the 56 min of runs is the wait `wsp-414` hides (parallel groups and bigger lists, from `wsp-362` and `wsp-363`), so the next group no longer sits behind it; overlap only shortens the run itself (suite is serial in one tree), unpriced until 3.1 | M | barrier run seconds per group at the same step count; with `wsp-414`, the next group's start against the previous barrier run's end |
| 3.9 | governor turns in the wall (`wsp-398`) | unknown: the report prices the governor as cost only; 14.5 min pass → start on group 3 → 4 is unexplained | S | governor seconds between a pass and the next start |
| 3.10 | name a command waiting on itself (`wsp-397`) | 6 min per occurrence (compounding-061), rate not yet counted: `wsp stuck <transcripts>` counts it, and was not run over the fleet's transcripts from the seat that built it | M | stuck-command flags per pass in wip, each with the seconds it held (built: `src/stuck.rs`, `repair::stuck_tick`) |

**3.5, what `wsp-393` found.** The clone was already one `clonefile(2)`; its cost
was the source. The trunk `target/debug/deps` held 157 745 leaked `*.rcgu.o`
codegen objects (rustc's per-unit intermediates, left by killed builds) among 4 088
real files. Measured on a scratch clone of that `target/`: 70 s to clone it whole
(28 s on a second, warm run), 0.57 s once those older than six hours were gone
(removing them was 68 s, once). `wsp` now removes them from the parent's
`target/` before cloning (20 s a tree until done), stages the clone outside the
tree, and gives a clone up at 90 s (half a cold build) with a line saying so.
Group 5's 88–333 s clones were that same entry count. **Not measured:** run end →
check started and spawn → claimed, before and after; the log's spawn lines are
dominated by re-spawns into warm trees, so the next pass's `effort.json` is
where they read. **Handed to `wsp-420`:** the 21 min of "starting". The 5 min
not-up rule (`untaken::look`, `wsp-335`) was in place and did not end it; it
acts only when `turn_began_since` is not `Some(true)`, and its clock is the check
entry's `opened`, so the first thing to read is that seat's `cycle.log` lines
and whether a turn was recorded on it.

Candidates the data does not support, with the reason:

- **Start the barrier run before every verifier has finished: dropped, Ed,
  2026-11-05.** It assumes no verifier blocks. When one does, the overlapped run
  is wasted and has to run again, which makes the turnaround worse (3 of the 11
  rows here were blocked once). The order stays: all verdicts, then the barrier
  run, then the check. The barrier run's wait is what `wsp-414` hides (parallel
  groups, bigger lists; `wsp-362`, `wsp-363`), not something to overlap with the
  verifiers. The time inside that order is §3.2, 3.8, 3.7 and the label.
- **Start the verifier while the row's own check runs**: worth 100–240 s a row
  (the shorter of the check and the verifier's read), with the same trade (one check in the twelve rows failed) and a cap place held meanwhile. Not filed, for Ed.
- **Start the verifier while the member writes its baton**: no row of the twelve
  has a late close in its timeline; there is nothing to save.
- **Let the barrier read verdicts already given instead of rereading**: the check
  seat is 136–290 s in total and mostly model time; at most a minute a group, and
  it cannot be separated from the check's judgement until 3.1 lands. Revisit then.
- **Warm seats**: seat up is 2–5 s already; the 32–115 s is the target clone and
  the claim (3.5), which a warm seat would hide at the price of a standing cap
  place, and the cap is the resource the rows already wait on (3.6).
- **Shorter ticks**: none of the twelve rows has a tick-sized gap; the poke works
  (hand-offs 1–8 s in compounding-102, compound-934 and compounding-061).
- **The governor skipping a turn when nothing needs judgement**: not priced; the
  report has no governor wall figure (3.9 gives it one first).

## 4. Migration: the first improvement stage

React work runs as passes with an improvement stage between. Rows in the first
stage, in order of time per unit of effort, each with the figure in the next
pass's effort report:

| row | stage 1 | figure that shows it worked |
|---|---|---|
| `wsp-389` | yes (filed, running) | member Bash seconds median down from 400–2 549; the verifier's read near 110–180 s |
| `wsp-392` one check per review tip | yes | `task-checked` per review is 1; review → verifier spawn ≈ `took` of one check |
| `wsp-390` name what ran | yes (every other figure reads through it) | unnamed waits under 5% of gaps; a `barrier run` phase with its start; the cap named for the waiting row |
| `wsp-391` one barrier run per tip | yes | one `barrier run` line a group |
| `wsp-393` fast check start | yes | run end → check started ≤ 20 s; spawn → claimed under 20 s |
| `wsp-394` cap holders named | yes, cheap | cap seconds a group with no finished holder |

The barrier run's 56 min (§1) is not a stage-1 or stage-2 saving: it is the wait
`wsp-414` hides, through parallel groups and bigger lists (it picks up `wsp-362`
and `wsp-363`), so those rows are where it is priced and measured, and `wsp-395`
below only shortens the run.

Stage 2, after stage 1's report gives the figures they need: `wsp-395` (per-step
seconds), `wsp-396` (Ed decides whether a project turns auto-pass on, after the stop-checks are
written), `wsp-398`, `wsp-397`.

Where this meets the split into components (`wsp-378`): a crate or component
that builds and tests alone shrinks member Bash (3.4) and the barrier's suite
(3.8) directly, so its figures are the same ones; and the barrier's per-step
seconds (3.1) are what a component's test selection is checked against.

## 5. The rows

On `delivery-time-cut`, not started (status draft); group 1 is the first stage.
`wsp-389` is the other first-stage row and stays on `delivery-time`.

| id | row | stage |
|---|---|---|
| wsp-390 | Timeline and effort name what ran | 1 |
| wsp-391 | One barrier run per group tip | 1 |
| wsp-392 | One wsp check per review tip | 1 |
| wsp-393 | Check and member seats start fast | 1 |
| wsp-394 | Cap holders named, finished ones shown | 1 |
| wsp-395 | Overlap independent barrier steps | 2 |
| wsp-396 | Stop-checks so auto-pass can pass | 2, Ed's call |
| wsp-397 | Name a command waiting on itself | 2 |
| wsp-398 | Governor turns in the wall | 2 |

## What is not known

- Whether the extra copies of a barrier run come from a liveness test that
  reads a live run as dead (the log says "ended without a result" 42 s in): read
  from the log, not from the code, and `wsp-391` starts there.
- The split of the 36% of member wall the report does not assign to gaps, tool
  or model time (`wsp-390`).
- How often a command waits on itself; one case is on record.
