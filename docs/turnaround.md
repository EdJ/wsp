# wsp turnaround

This doc measures how long a wsp row takes from start to landed, so the cuts that would shorten it can be ranked in minutes per row. The evidence and history are in docs/turnaround-handoff.md. Each section is written by its own row in list turnaround.

## Where a row's time goes

Written by wsp-429 on 2026-11-24, on omarchy (i9-14900KF, 32 threads). Row ids are this store's.

**The finding.** On omarchy, wsp does not show the slowdown Ed saw on his Mac. A wsp row takes 9.5 min from start to landed at the median (14.1 at p75). A compound-react row takes 23.9 min (44.8 at p75).

The stages are ranked by the median per row, with the p75 beside it for the tail (the gap table below). **Ranked that way, wsp loses the most minutes per row against compound-react at the group barrier: a row's share is 1.4 min against 1.2 at the median (+0.2), and 5.4 against 3.2 at p75 (+2.2).**

Two other stages are slower on wsp at the median:
- `wsp check` reads 0.7 against 0.3 min (+0.3), a larger gap than the barrier's. That gap comes from rows with no check stage, which count as 0: 12 of the 44 compound-react rows had none, against 6 of the 101 wsp rows. Among rows that had a check stage, wsp's check is faster: 0.7 / 1.1 min against 0.9 / 3.5. So the ranking sets the check-less rows aside for this stage.
- Seat start is slower by 1 s.

Caveat on the mean: it is not used to rank. compound-react's held barriers skew it, and by the mean wsp is 11.4 min ahead at the barrier (5.5 against 16.8 min per row).

On wsp's lists other than mp-coord, the barrier share is 5.3 / 9.3 min per row, mostly check seats running the full workspace suite by hand.

Building and testing is a small part of a wsp row. At the median, the row's own `wsp check` takes 0.7 min, the verifier's checks 0.1 min and the barrier share 1.4 min. Together that is about 2.2 of the 9.5 min. The agent's own work is 6.3 min.

### Stage medians and p75

Times are in minutes unless marked s. Each figure is a median / p75 over rows, or over groups and runs where the stage says so. "Load" is the 1-minute load average out of 32 cores, as median / p75. It is read from the samples nearest the stage:
- seat start, the agent's work and the check: the row's own `wsp check` at its start;
- the verifier: that check at its end;
- rebase and land: the check wsp runs on the landed commit;
- the barrier: the barrier run's start line.

The effort reports carry no load figure; each says "missing: the record carries no load figure".

| Stage | wsp, median / p75 | load | compound-react, median / p75 | load |
|---|---|---|---|---|
| start → landed, raw | 9.9 / 14.9 |  | 25.9 / 49.2 |  |
| held (blocked, parked, off the list, machine off), taken out | 0.0 / 0.0 |  | 0.0 / 0.1 |  |
| **start → landed, held taken out** | **9.5 / 14.1** | 2.3 / 4.7 | **23.9 / 44.8** | 2.6 / 4.6 |
| seat start: spawn → first tool call | 6 / 7 s | 2.3 / 4.7 | 5 / 6 s | 2.6 / 4.6 |
| the agent's own work: first tool call → review | 6.3 / 10.1 | 2.3 / 4.7 | 10.1 / 26.3 | 2.6 / 4.6 |
| `wsp check`: review → result (the row's wall, every run; 0 for a row with none) | 0.7 / 1.0 | 2.3 / 4.7 | 0.3 / 2.0 | 2.6 / 4.6 |
| &nbsp;&nbsp;the same, rows that had a check stage (95 wsp, 32 compound-react) | 0.7 / 1.1 | 2.3 / 4.7 | 0.9 / 3.5 | 2.6 / 4.6 |
| &nbsp;&nbsp;one check run (`took`), per run | 30 / 50 s | 2.3 / 4.7 | 37 / 101 s | 2.6 / 4.6 |
| verifier: spawn → verdict, all of it | 1.3 / 2.2 | 2.9 / 5.4 | 2.7 / 6.8 | 4.0 / 5.6 |
| &nbsp;&nbsp;its seat start | 4 / 6 s | 2.9 / 5.4 | 4 / 7 s | 4.0 / 5.6 |
| &nbsp;&nbsp;its checks (the builds and tests it ran) | 0.1 / 0.5 | 2.9 / 5.4 | 0.8 / 2.2 | 4.0 / 5.6 |
| &nbsp;&nbsp;reading and the verdict | 0.9 / 1.6 | 2.9 / 5.4 | 1.7 / 3.5 | 4.0 / 5.6 |
| rebase and land (`wsp review` + `wsp verified --holds`) | 2 / 6 s | 2.9 / 6.0 | 11 / 16 s | 1.1 / 1.8 |
| other waits (no stage above running) | 0.0 / 0.3 |  | 0.9 / 4.9 |  |
| send-backs: rework per row (send-back → next review; part of the stages above) | 0.0 / 1.8 |  | 1.2 / 5.5 |  |
| **barrier: the row's share (group wall ÷ members)** | **1.4 / 5.4** | 1.4 / 3.5 | **1.2 / 3.2** | 1.6 / 2.5 |
| &nbsp;&nbsp;group barrier wall: start → passed, per group | 3.4 / 7.0 | 1.4 / 3.5 | 1.6 / 4.6 | 1.6 / 2.5 |
| &nbsp;&nbsp;the barrier's own run, per run where a step ran | 44 / 72 s | 2.5 / 7.4 | 17 / 43 s | 1.7 / 1.9 |
| &nbsp;&nbsp;its check seat: seat start | 5 / 7 s | 1.4 / 3.5 | 6 / 7 s | 1.6 / 2.5 |
| &nbsp;&nbsp;its check seat: builds and tests run by hand | 73 / 134 s | 1.4 / 3.5 | 4 / 90 s | 1.6 / 2.5 |
| &nbsp;&nbsp;its check seat: spawn → its last turn | 232 / 378 s | 1.4 / 3.5 | 106 / 208 s | 1.6 / 2.5 |

The table covers 101 wsp rows in 42 groups (50 check seats) and 44 compound-react rows in 25 groups (26 check seats).

### The gap, stage by stage

wsp minus compound-react, in minutes, from the same rows. A plus sign means wsp is slower.

| Stage | median | p75 |
|---|---|---|
| seat start | +1 s | +1 s |
| the agent's own work | −3.7 | −16.1 |
| `wsp check`, every row (0 for a row with none) | +0.3 | −1.0 |
| `wsp check`, rows that had a check stage (6 wsp and 12 compound-react rows left out) | −0.2 | −2.5 |
| verifier, all of it | −1.5 | −4.6 |
| rebase and land | −0.2 | −0.2 |
| other waits | −0.9 | −4.6 |
| **barrier: the row's share** | **+0.2** | **+2.2** |
| start → landed, held taken out | −14.5 | −30.7 |

The means are left out because a few long waits pull them (see the outliers under "How the stages were cut"). compound-react's mean barrier share of 16.8 min, against wsp's 5.5, is mostly cr-linux-panes group 4, held overnight for 607 min.

wsp's lists other than mp-coord (51 rows) read as follows:
- start → landed 10.7 / 14.0;
- the agent's work 6.5 / 10.4;
- `wsp check` 0.6 / 0.9;
- verifier 1.4 / 2.6;
- barrier share 5.3 / 9.3 per row;
- group wall 5.6 / 14.0 per group (25 groups). Taken as a median over rows, as the share is, so that each group counts once per member, it reads 13.7 / 35.6;
- check load 2.5 / 6.6.

mp-coord's groups were sonnet rows, three to five to a group, so its barrier shares are small.

**Send-backs:**
- wsp: 47 of 101 rows were sent back at least once, 86 send-backs in all. 36 came from a failed `wsp check` and 50 from a verifier. Each cost 1.5 / 2.8 min of rework.
- compound-react: 28 of 44 rows, 63 send-backs (28 check, 35 verifier), at 1.7 / 3.6 min each.

The rework is part of the seat-start and work stages above, not added to them.

**The CPU clock was not limited** as far as anything records:
- wsp reads the clock only from `pmset` (`crates/trees/src/throttle.rs:337`), so omarchy keeps no record of it;
- `wsp throttle --json` reads `thermally_limited: false`, and the cpufreq `scaling_max_freq` equals `cpuinfo_max_freq` (5.7 GHz);
- the kernel log has no throttle line since 2026-11-19.

Load stayed low. The row checks read 2.3 / 4.7 on wsp, and the highest sample was 39.5 (wsp-372, during its loaded repeats).

### What wsp's own check runs

wsp declares no `## Checks`: `wsp check wsp --tiers` reads "nothing declared". compound-react declares none either. A row's check is therefore made of three things:
- its own `## Checks` lines;
- its done pieces' lines;
- the rule-derived steps in `src/check.rs`.

The rules:

| Rule | Command | Fires when | Builds the bin crate's test binary |
|---|---|---|---|
| `help` (`help_step`, wsp-564) | `cargo test the_help_is_what_it_was_before_the_registry` | a changed `.rs` is `src/main.rs`, `src/registry.rs` or `src/verbs.rs`, or holds `const VERBS` or `HELP_NOTES` | **yes**: with no `-p` at the root it builds every test target of the `wsp` package, which is the bin's unit-test binary and the 21 integration tests |
| `readonly` (`structure_steps`, wsp-569) | `cargo test readonly::tests` | a changed `.rs` is main, registry or verbs, or holds `const VERBS` | **yes**, the same build. It runs after `help`, so it finds the build done |
| `layering` (`structure_steps`, or `layering_step` for a new module, wsp-572) | `cargo test --test layering` | as `readonly`, or the diff adds a module | no: one integration target |

Most `cmd_*.rs` files declare `const VERBS`, so the three rules fire on almost every row. Each sat in 155 of wsp's 172 check runs, row and land checks together (87 of 101 rows):
- `help` paid for the build: 4 / 17.5 s, at most 39 s, 1,523 s in all;
- `readonly` then took 1 / 1 s, and `layering` 1 / 1 s.

Most rows' own lines are narrow, such as `cargo test -p <crate> --lib <module>::` or `cargo test --bin wsp <module>::`:
- 417 of them ran, at 3 / 12 s each;
- 153 of those also build the bin crate's test binary.

Overall:
- 61 % of the check seconds that ran (4,213 of 6,875 s) were in steps that build that binary;
- 91 of 101 rows' own checks built it;
- 4 steps, on two rows, ran the whole test binary: wsp-368's "full suite" (333 s) and wsp-307's `WSP_FULL_SUITE=asked-by-send-back cargo test --workspace` three times (63 to 195 s). wsp-163's own "bin unit tests" line, `cargo test --bin wsp -- tests:: readonly`, ran for 119, 238 and 241 s and selects nearly the whole bin test binary;
- the checks ran in a warm target 144 times and a cold one 12 times.

compound-react's row checks are mostly `npm run check` in `spikes/web-chrome` (vitest and Playwright), at 37 / 101 s per run.

### The barrier figures, and which barriers ran steps

**Caveat.** Since the manager's tree root came in, the barrier's own run on omarchy has read "0 step(s)", because it could not run. The record says "the barrier tree … is not under the tree root /mnt/data/src". This held from s2-state-core group 7 (2026-11-20 11:38Z) to omarchy-env group 1 (2026-11-24 09:15Z). In that stretch the check seats ran the suite by hand. Here that was 27 of the 42 wsp groups and 11 of the 25 compound-react groups.

The barriers that ran a step:
- wsp: s2-state-core groups 2, 4 and 6, read-api groups 1 and 3, trunk-green group 1 (one of its four runs), cr-events-1 group 1, cr-events-2 group 1;
- compound-react: cr-linux, cr-linux-2, cr-linux-5, cr-linux-7, cr-events-0 groups 1 and 2, cr-clean group 1.

The other runs had steps whose greens were all reused (0 ran), or could not run.

So most of the wsp barrier time here is the check seat's own builds and tests. The command that took the most seat time was `cargo test --workspace --no-fail-fast` (948 s summed), then loaded full runs of `cargo test --bin wsp` and the whole workspace.

The group wall also takes in waits on follow-ups and on the governor's decisions:
- read-api group 3: 120 min, held on its stop condition;
- verify-fix-2 group 1: 144 min;
- read-api group 1: 37 min, with follow-up wsp-501 run inside it;
- cr-linux-panes group 4: 607 min, held overnight.

This is why the p75 and the means sit far above the medians.

### How the stages were cut

For each row, its time from its list's "started" to "landed" (`wsp worklist show <list> --json`, member_stages) was split into stages. Each moment counts once, to the first stage in this order that holds it.

1. **Held.** A blocked, parked, dropped or not-started stretch, up to the next started, working or review. Also the hours omarchy was off: 2026-11-20 16:47–18:00Z, and 18:18Z to 11-23 07:01Z (`journalctl --list-boots`).
2. **`wsp check`.** cycle.log's "check <row>: check owed" up to its `task-checked` event in events.jsonl, which carries each step's seconds and the load. A check with no "owed" line was run by the seat itself and falls in the agent's work. Nine wsp rows ran one, at 18 / 71 s.
3. **Rebase and land.** The duration of the member's `wsp review` call (it rebases onto the trunk) and the verifier's `wsp verified --holds` call (it lands), read from the seats' transcripts. wsp runs the check on the landed commit after `landed`, so that check falls outside start → landed. 15 wsp rows had one, at 44 s on average (0 to 119 s).
4. **The verifier.** cycle.log's "spawn <row> --agent --verify" up to its verdict or send-back. Within it: its seat start, its builds and tests (Bash calls running cargo, npm, npx, vitest or `wsp check`), and the rest.
5. **Seat start.** "spawn <row> --agent" up to the first tool call in that seat's transcript (`~/.claude/projects/*worktrees-<row>/`).
6. **The agent's own work.** The first tool call up to the next review.

What no stage holds is **other waits**.

The barrier is after `landed`, so it is reported apart, as the row's share: the group's wall from cycle.log's "started <list> · barrier N" to its passed event, divided by its landed members. The barrier run's steps and load come from cycle.log's "barrier run" line. The check seat's figures come from its transcript (`*worktrees-<list>-check-<n>/`).

The two series of bin-built steps differ: compound-react's `cargo` steps build web-chrome's Tauri crate, not wsp, and are not counted as the wsp bin.

Outliers sit in the means but hardly move the medians:
- wsp-307: 982 min of other waits, sitting at review in cr-events-3 before the shutdown;
- wsp-504 and wsp-445: mostly held;
- compound-react-062: 403 min, behind cr-linux-panes group 4's held barrier;
- compound-react-009: 15 verifier respawns.

The scripts and their outputs are in `$WSP_SCRATCH`, i.e. `~/.local/state/wsp/scratch/wsp-429/`:
- `collect.py` and `analyze.py` make rows.json and stages.json;
- `summary.py` and `table.py` give the figures, and `gaps.py` the gap table (it prints the means too).

### Rows used

These are rows that landed on omarchy from 2026-11-19 07:33Z to 2026-11-24 19:22Z. That stretch is the window of this machine's events.jsonl and cycle.log. The rows come from lists run the usual way: wsp spawns the members, verifies each and checks the barrier. Retros and spec-breakdown (docs only, on fable) are left out, and so are lists that ran before the log began (m0 to m3, s0, s1, overnight-*, attention-noise).

**wsp, 101 rows:**
- s2-state-core (15): 117, 118, 119, 120, 121, 122, 343, 123, 124, 125, 126, 346, 127, 128, 358
- read-api (11): 370, 371, 372, 501, 502, 373, 374, 375, 377, 378, 504
- hub-deploy (5): 489, 490, 491, 492, 493
- trunk-green (5): 368, 322, 351, 369, 395
- verify-fix (1): 426
- verify-fix-2 (3): 444, 445, 453
- omarchy-env (3): 361, 427, 428
- linux-seats (2): 151, 152
- mp-coord (50): 158, 409, 452, 458, 463, 477, 159, 160, 168, 410, 169, 411, 459, 478, 479, 171, 172, 412, 414, 460, 161, 175, 461, 163, 165, 173, 176, 462, 162, 174, 464, 185, 417, 465, 481, 482, 466, 416, 467, 468, 469, 484, 470, 483, 167, 180, 194, 418, 485, 486
- cr-events-1 (1): 304
- cr-events-2 (2): 305, 306
- cr-events-3 (3): 307, 308, 471

**compound-react, 44 rows:**
- cr-linux (2): 007, 012
- cr-linux-2 (2): 008, 009
- cr-linux-3 (1): 013
- cr-linux-4 (1): 015
- cr-linux-5 (1): 020
- cr-linux-6 (1): 021
- cr-linux-7 (2): 016, 017
- cr-deps (1): 019
- cr-events-0 (6): 024, 025, 026, 027, 035, 039
- cr-events-1 (3): 037, 038, 046
- cr-events-2 (1): 047
- cr-fixes (4): 055, 036, 048, 058
- cr-linux-panes (10): 010, 059, 060, 061, 064, 065, 062, 070, 011, 063
- cr-clean (3): 073, 074, 072
- cr-push (1): 023
- cr-scrub (1): 071
- cr-sync (3): 076, 077, 081
- cr-mac-clean (1): 087

### Against Ed's baseline

Ed's figures (hand-off §2, his store, his Mac) set against omarchy's:

| | Ed's Mac | omarchy |
|---|---|---|
| wsp start → landed, median / p75 | 24 / 43 min | 9.5 / 14.1 min |
| a row's own `wsp check`, per run | 96 / 227 s | 30 / 50 s |
| wsp barrier run | 378 / 574 s | 44 / 72 s where a step ran (the group wall is 3.4 / 7.0 min) |
| share of wsp rows sent back at least once | 42 % | 47 % |

The Mac was thermally limited to 40 % of its clock for long stretches, and its load reached about 800. Omarchy's clock was not limited, and its load stayed under 5 at the p75. So the crate split's case cannot rest on omarchy's build and test time: here that is about 2 of a row's 9.5 minutes. Measure (b), the inner loop, should time the split's gain on this machine before phase 2 is planned.

## The inner loop

Measured 2026-11-24 by wsp-430, in the row's own tree (`/mnt/data/src/private/wsp/.worktrees/wsp-430`, `target/`
in the tree), at wsp d70a4fa, on the person's machine (omarchy): i9-14900KF, 32 threads, 64 GB, NVMe. No sccache
on it (`.cargo/rustc-wrap` runs bare rustc), `lld` is installed but no linker override is set in `.cargo/config.toml` or `~/.cargo/config.toml`, so the link
is the toolchain's default. rustc 1.98.1, the `dev`/`test` profile (opt-level 0, debuginfo, **incremental on**, 16 codegen
units).

**Load.** `load1` is `/proc/loadavg` read just before each run; every figure below was started at load1 below 4
(0.3 to 3.4; the row's own previous build is part of it). A first attempt at the cold build
was started at 5.18 and refused by the cargo shim anyway; the run reported is the second, at 3.41.

**Clock.** Governor `powersave` under `intel_pstate` (active), energy preference `performance`, `scaling_max_freq`
5.7 GHz = `cpuinfo_max_freq`: the clock was **not limited by policy**. The mean of the 32 threads' MHz read at
each start was 1.8 to 4.4 GHz (most threads idle, so it says little). The package sensor read 99 to 100 °C (high
80, crit 100) at the start of the step 1 runs 4 and 5 and 53 to 85 °C elsewhere; the figures of those two runs
(4.48, 4.37 s) equal the cool ones (4.47), so a thermal limit did not show, but the throttle counters in
`/sys/devices/system/cpu/*/thermal_throttle` were not read before and after each run, so it is not excluded.

**Method.** "Warm" is a build of the same command just before, then one line edited, then the command. The edit
is a constant no test reads: `WAKE_CAP_SECS` in `src/cycle.rs` (`10 * 60` to `10 * 6N`, a different N each run),
and the message string `"not packing: a commit is in flight there"` in `crates/trees/src/gc.rs` (a digit added,
a different one each run). Build and test run are timed apart with `cargo test … --no-run`, then the same
command without it. The three runs of a figure are in the order run.

| Step | Command | Run 1 | Run 2 | Run 3 | Load1 at start (1, 2, 3) |
|---|---|---|---|---|---|
| 1a. edit `src/cycle.rs`, build | `cargo test --bin wsp cycle:: --no-run` | 4.48 s | 4.37 s | 4.47 s | 2.33, 2.65, 2.50 |
| 1b. then the test run (build is a no-op) | `cargo test --bin wsp cycle::` | 12.20 s | 12.08 s | 12.07 s | 2.46, 2.60, 2.46 |
| 2a. edit `crates/trees/src/gc.rs`, build | `cargo test -p wsp-trees --lib gc:: --no-run` | 0.76 s | 0.67 s | 0.68 s | 2.23, 2.13, 2.13 |
| 2b. then the test run (4 tests) | `cargo test -p wsp-trees --lib gc::` | 0.67 s | 0.67 s | 0.66 s | 2.23, 2.13, 2.13 |
| 3a. bin test-binary build after the step 1 edit | `cargo test --bin wsp --no-run --timings` | 4.66 s (cargo: wsp unit 4.17 s) | 4.37 s (4.14 s) | 4.47 s (4.19 s) | 1.82, 1.83, 1.77 |
| 3b. the same, rustc's own passes (`RUSTC_BOOTSTRAP=1`) | `cargo rustc --profile test --bin wsp -- -Z time-passes` | 4.65 s wall, rustc total 4.40 | 4.56 s, 4.31 | 4.57 s, 4.29 | 2.11, 2.10, 2.09 |
| 3c. of which front end | parse, expand, resolve, type check, borrow check, checks | 1.58 s | 1.56 s | 1.56 s | |
| 3d. of which codegen | mono collection, CGU partition, LLVM IR 0.74 / 0.73 / 0.76, LLVM passes 0.19 | 0.93 s | 0.92 s | 0.95 s | |
| 3e. of which incremental state written | `serialize_dep_graph` | 0.60 s | 0.64 s | 0.59 s | |
| 3f. of which link | `link_binary`, one `ld` | 0.49 s | 0.49 s | 0.50 s | |
| 3g. of which rustc start-up, metadata, dep-graph load | rest of the total | 0.81 s | 0.69 s | 0.69 s | |
| 4. `wsp check --affected --run` after the step 1 edit | one step, `crate:wsp`: `cargo test -p wsp`, build included | 241.17 s | 242.53 s | 245.05 s | 1.94, 0.88, 0.27 |
| 5. the full suite | `cargo test --workspace` (the plan names no other steps), build after one edit included | 332.58 s, all passed | 333.27 s, all passed | 328.56 s, one test failed (`--no-fail-fast`) | 1.14, 1.20, 1.79 |

Once:

| | Figure | Load1 |
|---|---|---|
| Cold `cargo test --workspace --no-run` (no `target/`, 75 crates compiled) | 61.93 s, one run | 3.41 |
| Tests the cuts must keep: `cargo test --workspace -- --list \| grep -c ': test'` | **4102** (0 benchmarks) | |

### What it says

- **One edit in the bin crate costs 4.4 s of build and 12 s of tests** (`cycle::`); an edit in `wsp-trees` costs
  0.7 s of build and 0.7 s of its test. The bin crate costs about six times a small crate for a one-line edit.
- **The split of the 4.4 s (step 3).** The front end is 1.6 s, 36 %. Codegen (mono collection, partition, IR and
  LLVM) is 0.9 s, 21 %, small because incremental reuse leaves most CGUs alone. Writing the incremental state is
  0.6 s, 14 %, and the link 0.5 s, 11 %. What is left, 0.7 to 0.8 s, 17 %, is start-up, loading the crate
  metadata and the previous dep graph. The macro expansion alone is 0.68 s of the front end.
  `cargo --timings` gave one duration for the `wsp` unit (4.14 to 4.19 s) and no front-end or codegen sections,
  so the split is rustc's `-Z time-passes`. It needs a nightly flag: `RUSTC_BOOTSTRAP=1` on 1.98.1 gives it
  without a nightly toolchain, and the build is not the one the checks run (the `cargo rustc` flags differ), but
  its total (4.29 to 4.40 s) is within 0.3 s of step 3a.
- **`wsp check --affected --run` after an edit in `src/cycle.rs` runs one step, `cargo test -p wsp`: 242 s.**
  That is the whole root package (the bin's 2,831 unit tests, 142 s, and the root `tests/*.rs` binaries), not
  `cycle::`. The edit reaches only crate `wsp`, so the affected set is already the narrowest crate-level one.
  The row has no narrower step, so the check cannot do better than the crate until it can run a module.
- **The full suite is 330 s, of which 142 s (43 %) is the bin crate's unit-test binary** (2,831 tests, one binary).
  The binaries run one after the other: the sum of their `finished in` is 327 s of the 333 s wall; the build
  after one edit is about 5 s. The slowest ten (run 1; the other runs are within 5 s of each):

| Test binary | Seconds (run 1) |
|---|---|
| `wsp` unit tests (`src/main.rs`) | 142.33 |
| `server` (`crates/server/tests/server.rs`) | 41.17 |
| `wip_api` | 25.15 |
| `standalone` | 20.28 |
| `wsp_trees` unit tests | 17.26 |
| `mirror` | 12.28 |
| `hub_sync` | 10.92 |
| `hub_cache` | 7.27 |
| `cc_wrap` | 7.03 |
| `privacy` | 6.46 |

### Things the figures hide

- **The first run of a command after a different command is not warm.** The first pass of steps 1 and 2 (which
  was discarded and redone with a warm-up) read 34.80 s for the first `cargo test --bin wsp … --no-run` after the
  cold `--workspace` build, then 4.57 and 4.38 s: cargo unifies features per command, so `--bin wsp` rebuilt
  `wsp-api`, `wsp-server`, `wsp-client` and `wsp-hub`. The same showed in step 2 (2.78 s, then 0.68 and 0.68 s).
  A seat that alternates `-p` and `--workspace` pays that each time.
- **Incremental compilation is on.** These are loop figures. A barrier's tree is warm and moved in place, so it
  is incremental too; a fresh checkout is the 62 s cold figure, not the 4.4 s one. A non-incremental build of the
  bin crate alone was not measured.
- **The suite has flakes at load below 2.** Two of five full runs stopped at the first red test binary (a plain
  `cargo test --workspace` stops there): after 145.74 s on
  `cmd_initiative::tests::plan_assembles_the_order_from_the_doc_and_a_dry_run_seats_nothing` (its two plans differ
  only in a `/tmp/wsp-t-…-<pid>` path from another test's directory) and after 163.06 s on
  `hub_cache::doctor_on_a_logged_in_spoke_names_the_hub_the_user_the_last_sync_and_the_journal`. Those two runs
  are not in the table (they did not finish); both started at load1 1.31 and 0.94. The third red run in the table
  failed `mirror::tests::a_pass_where_only_machines_json_moved_reads_the_machines_alone` and finished because it
  ran with `--no-fail-fast`. So the count of tests the cuts must keep is of tests listed, not of tests that pass
  every time. Each of the three passed when run alone afterwards; they are named here for whoever owns them, not
  fixed in this row.
- **`WSP_FULL_SUITE`.** The shim refuses `cargo test --workspace` in a seat without a reason; every run above
  was made with the reason "wsp-430: turnaround measure (b) …", as the packet orders.

### After the phase 2 cut (wsp-440)

Measured 2026-11-25 at 0f21ae1, in wsp-440's tree, on the same machine and toolchain as above. The engine and the
seats are one crate, `crates/engine` (`wsp-engine`). The edit is the same constant, `WAKE_CAP_SECS`, now in
`crates/engine/src/cycle.rs`, a different N each run. Each command had one warm-up build just before its three runs.
Times are wall clock of the whole cargo call. `load1` was read just before each run, all below 4 (the climb past 3
follows the `-p wsp-engine` warm-up; no cargo or rustc of another seat was seen before the first pass).

| Command, after the edit | Run 1 | Run 2 | Run 3 | Load1 at start (1, 2, 3) | Of 4.4 s |
|---|---|---|---|---|---|
| `cargo test --no-run` (the root package's test targets) | 3.84 s | 3.83 s | 3.75 s | 1.59, 1.62, 1.89 | 87 % |
| `cargo test -p wsp-engine --no-run` | 3.34 s | 3.35 s | 3.56 s | 3.90, 3.75, 3.53 | 76 % |
| `cargo test --bin wsp --no-run` (wsp-430's step 1a command) | 3.05 s | 2.93 s | 2.96 s | 3.33, 3.14, 3.14 | 67 % |
| wsp-430, before: `cargo test --bin wsp cycle:: --no-run`, edit in `src/cycle.rs` | 4.48 s | 4.37 s | 4.47 s | 2.33, 2.65, 2.50 | 100 % |

- **The cut did not meet half of the bin-crate figure** (under 2.2 s). The best of the three commands is 2.9 to 3.1 s,
  and the engine's own test build is 3.3 to 3.6 s.
- **The prediction for one crate held:** 3.4 s ("The cuts, ranked", cut 3) against 3.35 s median measured.
- **Seats** are in the same crate, so an edit in a seats module rebuilds the same unit; it was not timed apart. The
  1.5 s predicted for a seats crate of its own does not apply.
- `cargo test --no-run` at the root does not build the engine's own tests. It rebuilds the engine library, then the
  root package's test targets, so it is not the same work as either of the other two.
- An earlier pass the same morning (loads 0.4 to 1.8) lost its wall times to a script fault. Cargo's own `Finished`
  times from it: 3.37, 3.77, 4.01 s; 3.20, 3.16, 3.17 s; 2.76, 2.67, 2.69 s, in the table's order.

## The cuts, ranked

Written by wsp-431 on 2026-11-24, from the two sections above and wsp-429's 101 wsp rows (its `stages.json` and
`rows.json`). This row added three counts from the same rows:
- the reason each send-back gives in its row's log (`wsp show <row> --log`);
- the cargo calls in the agent seats' own transcripts (the verifier's seats left out);
- the tests each barrier's check seat ran, per group.

The scripts are in `~/.local/state/wsp/scratch/wsp-431/`: `sb.py` (send-back reasons), `agent.py` (the seats'
cargo calls) and `rest.py` (check, verifier, barrier and load).

**The finding.** Building and testing is about 4 of a row's 9.5 minutes, not the 2.2 that wsp-429's stages show.
The agent's own seats run cargo for 2.0 / 3.3 min (median / p75) of their 6.3 / 10.1 min of work. Most of that
time is running tests, not compiling. The compile after an edit is the only part phase 2 cuts. It is 9 / 20
bin-crate builds per row at about 4.4 s each, so 40 / 90 s. **Phase 2 can take at most 0.3 / 0.7 min off a
row.** Two cuts take more at the median, and neither needs a crate:
- start the verifier beside the check, not after it: 0.60 / 0.93 min;
- the barrier's own run working again, so its check seat stops running the suite by hand: 0.35 / 1.56 min. wsp-361's
  fix has already landed.

wsp-440's gate wants a build under half of the bin crate's 4.4 s, so under 2.2 s. A straight line through wsp-430's
two figures predicts 2.4 s for an edit in engine, a narrow miss. One crate for engine and seats predicts 3.4 s.
Measured after the cut, one crate: 3.35 s for `-p wsp-engine`, gate not met ("After the phase 2 cut", above).

### The ranking

How to read the figures:
- **Minutes per row** is what the cut would have taken off each of the 101 rows, from the stage it acts on, as median
  / p75. The mean is in brackets where the median is 0 or the tail carries the cut.
- **≤** marks an upper bound.
- **Rank** is by the median, then the p75. Rows 9 and 10 are bounds only, so they are ranked last.

The savings do not add: rows 2 and 6 overlap, and so do rows 9 and 10.

| # | Cut | Stage it saves from | Min per row, median / p75 (mean) | The figure it rests on | Where it stands |
|---|---|---|---|---|---|
| 1 | The verifier starts beside the row's `wsp check`, not after it | check + verifier, one after the other | 0.60 / 0.93 (0.79) | The verifier spawns 0 / 1 s after the check result (120 spawns). The check takes 0.7 / 1.0 min and the verifier 1.3 / 2.2 min (wsp-429) | not a row |
| 2 | The barrier's own run works again, so the check seat stops running the suite by hand (part of the barrier's share) | barrier share | 0.35 / 1.56 (1.21) | The check seat's by-hand builds and tests ÷ the group's members. 27 of 42 groups were in wsp-361's 0-step stretch | wsp-361's fix landed. wsp-442 confirms it |
| 3 | Phase 2: `engine` and `seats` as two crates (wsp-432 to wsp-441) | the agent's own work | ≤ 0.30 / 0.67 (one crate: ≤ 0.15 / 0.35) | Engine edit predicted at 2.4 s against 4.4 (wsp-430 steps 1a and 2a, scaled by lines), × 9 / 20 bin builds per row | rows on this list |
| 4 | Narrower check steps (wsp-433) | check, repeat barriers | ≤ 0.1 / 0.1 (≤ 0.6) | Whole-binary row steps (3 rows, 18 min over 30 s). Repeat barrier seats (6 of 41 groups, 32 min) | wsp-433, group 2 |
| 5 | Thermal and load: the throttle, the check cap | check | 0.07 / 0.11 on omarchy (thermal: 0) | The `help` step takes 5.5 s at load < 4 and 11.5 s at ≥ 4. The clock was not limited | no row |
| 6 | Shard or split the test binary, apart from the crate split | barrier, the agent's own work | 0.0 / 0.4 (0.3). 0.2 / 1.2 if seats keep running the suite by hand | Full suite 330 s with the binaries run one after another. The bin's unit-test binary is 142 s of it (wsp-430) | not a row |
| 7 | One feature set across cargo commands | the agent's own work | 0 / ≤ 1.0 (≤ 0.5), low confidence | First `--bin wsp` build after `--workspace` took 34.8 s against 4.4 (wsp-430). Seats switch 0 / 5 times per row | not a row. Measure first |
| 8 | Limit send-backs: wsp-386 (no headroom), with wsp-364 (a limit judged against a cold check) already landed | send-back rework | 0 / 0 (0.15), less after wsp-364 | 7 of the 91 logged send-backs were for a limit alone, on 6 rows, costing 15.6 min of rework | wsp-386 open, wsp-364 done |
| 9 | Groups run beside a held barrier (wsp-443) | barrier share (the next group's wait) | ≤ 1.44 / 5.58 (≤ 5.48) | The barrier share. cr-linux-panes group 5 sat about 15 h behind group 4 | needs Ed |
| 10 | A barrier passes without a check seat when its run is green (the rest of the barrier's share) | barrier share | ≤ 0.56 / 2.21 (≤ 4.24) | The share less the by-hand tests and the barrier's own run | not a row. The person's call |

### Each cut

**1. The verifier beside the check.**
- How it runs today: the verifier is spawned when the row's check passes, 0 s after the check result at the median
  and 1 s at p75 (120 verifier spawns). The row waits for the check (0.7 / 1.0 min) and then the verifier (1.3 / 2.2
  min).
- The gain: spawned at `wsp review` instead, the verifier saves the shorter of the two. Per row that is 0.60 /
  0.93 min (mean 0.79).
- The cost: when the check fails (36 of the 86 send-backs), the early verifier's seat is spent for nothing. Its
  verdict would also have to wait for the check's result before it can land.
- Its own checks (0.1 / 0.5 min) re-run what the check ran. Reusing those greens would save at most that.
- It is below a minute per row, so wsp-442's rule would not file it. It is named here for the governor.

**2. The barrier's own run, working again.**
- The barrier share is 1.44 / 5.58 min per row (mean 5.48).
- From s2-state-core group 7 to omarchy-env group 1, the barrier's run read 0 steps. The check seat ran the builds
  and tests by hand instead. Divided among the members, that is 0.35 / 1.56 min per row (mean 1.21).
- wsp-361's fix (3d32fc4, landed 2026-11-24; the row's group barrier is still to pass) ended that stretch. omarchy-env
  group 1, at 09:15Z that day, was its last group. The barrier's run reuses greens: 44 / 72 s where a step
  ran.
- The saving holds only if check seats stop running the suite by hand once the run has run it. wsp-442's stage
  split, on rows landed after the fix, is where that shows.

**3. Phase 2: `engine` and `seats`.**
- **wsp-430's figures:** a one-line edit, then `cargo test --no-run`, takes 4.4 s in the bin crate (`src/`, 213k
  lines today) and 0.7 s in `wsp-trees` (18.5k lines).
- **A straight line through those two:** 0.35 s plus 0.019 s per 1,000 lines. The parts of the 4.4 s that grow with
  the crate are the macro expansion (redone on every build), the rest of the front end, writing the dep graph and
  the link. Together they are about 70 % of it (step 3).
- **docs/components.md's map today:** `engine` is 106k lines with 1,487 `#[test]`s. `seats` is 58k lines with 734
  (`ended_panes` and `sync` are gone). About 47k lines stay in the bin: timeline 13k, surface 28k and top 5k.
- **Predicted `cargo test --no-run` after one edit:**
  - in engine: 2.4 s, 54 % of 4.4;
  - in seats: 1.5 s;
  - in what is left of the bin: 1.3 s;
  - in one engine+seats crate (164k lines): 3.4 s, 77 %.

  wsp-440's gate is under half, 2.2 s. Two crates miss it by 0.2 s, which is within what a straight line from two
  points can say, so wsp-440's own timing decides. One crate misses it plainly.
  (wsp-440 cut one crate and measured 3.35 s; seats, being in it, have that figure, not 1.5 s.)
- **Who gets the gain:** only a seat that runs `cargo test -p wsp-engine …`. A `cargo test --bin wsp …` after an
  engine edit builds engine and then the bin, about 2.4 + 1.3 s, which is near today's 4.4 s.
- **What phase 2 does not shorten:**
  - the check's `help` and `readonly` rules: where they fire, they build the root package's test targets, and those
    rebuild on top of a changed engine;
  - the verifier's checks;
  - the suite: the same 4,102 tests run, they only move between binaries.
- **Per row:**
  - Agent seats ran 9 / 20 bin-building cargo calls per row (mean 14.8; `agent.py`). At 2.0 s saved per call, that is
    18 / 40 s, so 0.30 / 0.67 min.
  - It is an upper bound, because not every call follows an edit.
  - One crate saves about 1 s per call: ≤ 0.15 / 0.35 min.
- **The cost:** ten rows, two of them large, with serial cuts and installs between them.

**4. Narrower check steps (wsp-433).**
- **A row's own check is already narrow.** 417 of the rows' own lines ran, at 3 / 12 s each. Three rows ran a whole
  binary:
  - wsp-368's full suite, 333 s;
  - wsp-307's `cargo test --workspace`, 63 to 195 s, three times;
  - wsp-163's `--bin wsp -- tests:: readonly`, 119 to 241 s, three times.

  Their time past 30 s is 18.2 min over 101 rows, a mean of 0.18 per row. The median and p75 are 0.
- **Repeat barriers.** 6 of 41 groups seated a second check seat. The later seats ran 31.8 min of tests by hand:
  hub-deploy 856 s, cr-events-3 413 s, read-api 411 s and trunk-green 221 s. That is a mean of 0.32 per row. A second
  barrier that ran only what changed since the first would keep a small part of it.
- **The `help` and `readonly` rules** build every test target of the root package, which includes the debug binary
  the 21 integration tests run. Naming `--bin wsp` would build the unit binary alone. Not measured: at most one bin
  build of about 4 s per check run where they fire, so ≤ 0.1 min per row at 1.7 runs.
- **Total:** ≤ 0.1 / 0.1 (≤ 0.6).
- **A trap:** for a `src/` edit, `--affected` runs `crate:wsp` (`cargo test -p wsp`, 242 s; wsp-430 step 4). A row's
  own module line takes 3 / 12 s. If wsp-433 swapped a row's module lines for the crate step, that row's check would
  get about 4 minutes slower.

**5. Thermal and load.**
- **Thermal:** omarchy's clock was not limited (wsp-429, wsp-430), and wsp reads the clock only from `pmset`. The
  throttle's check cap saves nothing here: 0.
- **Load:** the `help` step, which pays the bin build, took 5.5 s at the median at load < 4 (78 runs). At load ≥ 4 it
  took 11.5 s (38 runs).
  - A row's bin-building check steps are 0.43 / 0.67 min.
  - A third of them run at twice the time, so holding row checks below load 4 would save about 0.07 / 0.11 min per
    row.
- **Ed's Mac:** 96 s per check run against 30 s here, at 1.7 runs per row, is about 1.9 min per row. That can be
  measured only on that machine.

**6. Sharding or splitting the test binary.**
- **The suite:** 330 s. Its binaries run one after another (their times sum to 327 s of the 333). The bin crate's
  unit-test binary is 142 s of it.
- **The gain per full run:** run side by side (nextest, or one `cargo test --test …` per binary in parallel), the
  wall falls toward the longest binary, about 142 s. That saves about 3 min. Splitting the 142 s binary itself
  helps only past that point, and its slowest single test was not timed.
- **Who runs the full suite:**
  - The agent seats' own `--workspace` runs: 0 / 39 s per row (mean 32 s). Sharding saves 0 / 0.4 min (mean 0.3).
  - Check seats by hand: 0.35 / 1.56 per row (cut 2). Sharding saves 0.2 / 0.9 of that, but those by-hand runs go
    anyway if cut 2 holds.
  - `wsp check --affected`'s `crate:wsp` step would fall from 242 s to about 142 s.

**7. One feature set across cargo commands.**
- **The figure:** wsp-430's first `cargo test --bin wsp --no-run` after a `--workspace` build took 34.8 s, against
  4.4. Cargo resolved features for that command and rebuilt `wsp-api`, `wsp-server`, `wsp-client` and `wsp-hub`.
- **How often seats hit it:** they switch between `--workspace` and narrower cargo commands 0 / 5 times per row
  (mean 3.1, in 50 rows).
- **The estimate:** cargo keeps both builds in `target/`, so the cost is probably paid once per side per tree, not
  at every switch. That gives ≤ 1 min for a row that switches: 0 / ≤ 1.0 (≤ 0.5).
- **Not measured:** the dev-dependencies that turn on `test-support` (root `Cargo.toml` lines 34 to 45) are a likely
  cause, but not a checked one. A timed switch in each direction would settle the figure before anyone files a row.

**8. Limit send-backs (wsp-386, wsp-364).**
- **The count:** wsp-429's 101 rows logged 91 send-backs. Seven of them, on six rows, were for a limit alone ("took its
  whole budget", no test failing):
  - wsp-122, wsp-343, wsp-124 and wsp-127, all 2026-11-20;
  - wsp-368 and wsp-351 twice, 2026-11-23.
- **The cost:** 1.0 to 4.3 min of rework each, 15.6 min in all. That is a mean of 0.15 per row; the median and p75
  are 0. compound-react had 2 of its 69 (compound-react-038 and -060), 1.5 min.
- **wsp-364** landed as d7d9f99 on 2026-11-23 at 07:53Z. It judges a limit after cargo's `Finished` line, which
  takes out a cold build (wsp-122's tree was cold) and a warm tree's incremental build. Five of the seven came before
  it landed. wsp-351's two came after (08:16Z and 08:25Z), and the record does not say whether the daemon ran the new
  build by then. No row's log has a limit-only send-back after that.
- **What is left for wsp-386:** ≤ 0.15 per row, and likely less.

**9. Groups beside a held barrier (wsp-443).**
- **The bound:** at most the share of the held group, ≤ 1.44 / 5.58 per row (mean 5.48). It pays only where the next
  group has no `after` into the held group and no file in common with it. This row did not count those groups.
- **Where it pays:** in the mean, not the median. cr-linux-panes group 5 waited 302 min of by-hand checks plus a
  604 min hold behind group 4, though neither of its members rests on group 4's row.
- **On turnaround itself** the groups are serial (cut rows wait for installs), so it saves nothing here.
- **Status:** it needs Ed's answer first.

**10. A barrier without a check seat.**
- **What is left of the share:** after the by-hand tests and the barrier's own run, the share is 0.56 / 2.21 per row
  (mean 4.24). That is the check seat's reading plus the waits on follow-ups and decisions.
- **The design:** a barrier whose run is green, and whose members share no file, could pass without a seat.
- **Why it is the person's call:** the seat is there to catch members that undid each other. It is not a row.

### The rows behind this group

The barrier applies these lines with a `--why`. This row edits no list.

| Row | Call | Why |
|---|---|---|
| wsp-432 | keep | Its one-or-two-crate choice now decides whether phase 2 earns its rows. Two crates predict 2.4 s per engine edit (≤ 0.3 / 0.7 min per row). One crate predicts 3.4 s (≤ 0.15 / 0.35), which fails wsp-440's half gate before any seam is paid for. |
| wsp-433 | keep | It is the only cut on this list that reaches the barrier (repeat barriers, whole-binary steps), and it does not wait on phase 2. It must keep a row's module lines and not swap in `crate:wsp` (242 s against 3 / 12 s). |
| wsp-434 | keep | Mechanical and cheap. The 38 `-> main:` edges block every cut, surface and timeline included. |
| wsp-435 | keep | Its edges run from seats and engine up into top and surface, so they leave either shape of phase 2. |
| wsp-436 | keep | The same: engine and seats into timeline, under either shape. |
| wsp-437 | keep under two crates; drop under one | Under one crate its edges are internal. The merged crate's predicted 3.4 s cannot meet wsp-440's gate, so narrowing it buys nothing measurable. |
| wsp-438 | keep under two crates; drop under one | The same reason as wsp-437. |
| wsp-439 | keep | `memory` and `disk` must leave the root under either shape. Under one crate, only its `attention -> cmd_watch` edges become internal. |
| wsp-440 | keep | The predicted 2.4 s against a 2.2 s gate is too close to settle without the cut, so its own timed figure decides. Its hold on missing the gate stands. |
| wsp-441 | keep under two crates; drop under one | It is predicted at 1.5 s per seats edit once engine is out, and the list already drops it under one crate. |
| wsp-442 | keep | It re-measures on the same machine. Its re-rank should also carry cuts 1, 2 and 10 here, which neither the hand-off nor its overview names. Cut 2 needs its stage split, on rows landed after wsp-361's fix, to confirm it. |

## After phase 2

Measured 2026-11-25 by wsp-442 (leg measure) on the Linux host, the same machine and toolchain (rustc 1.98.1) as the
"before" figures. The re-rank, below, is leg rank's, with the figures it added the same day.

### The inner loop, before and after

wsp-430's five steps at df704d8 (phase 2's one-crate cut and wsp-522), in wsp-442's tree, three runs each, 09:55 to
10:36 local. Each timed run started only at load1 below 3.5; the loads were 0.62 to 2.20. Each figure is wall time of
the whole command; "after" is the median of three, with the runs in order. The `src/cycle.rs` edit is now the same
constant (`WAKE_CAP_SECS`) in `crates/engine/src/cycle.rs`; the `gc.rs` edit is unchanged.

| Step | Command after the cut | Before (wsp-430, median) | After, median | Runs 1, 2, 3 | Load1 at start |
|---|---|---|---|---|---|
| 1a. edit cycle, build | `cargo test -p wsp-engine --lib cycle:: --no-run` | 4.47 s | **3.45 s** | 3.45, 3.45, 3.44 | 2.07, 1.75, 1.43 |
| 1b. the test run | `cargo test -p wsp-engine --lib cycle::` (186 tests) | 12.08 s | 11.96 s | 11.95, 11.96, 12.06 | 2.06, 1.69, 1.39 |
| 2a. edit gc, build | `cargo test -p wsp-trees --lib gc:: --no-run` | 0.68 s | 0.65 s | 0.65, 0.64, 0.65 | 1.24, 1.24, 1.24 |
| 2b. the test run | `cargo test -p wsp-trees --lib gc::` | 0.67 s | 0.66 s | 0.65, 0.66, 0.66 | 1.24, 1.24, 1.22 |
| 3a. bin test binary after the engine edit | `cargo test --bin wsp --no-run --timings` | 4.47 s | **2.85 s** | 2.95, 2.84, 2.85 | 1.51, 1.51, 1.47 |
| 3b. the same with `-Z time-passes`, bin | `cargo rustc --profile test --bin wsp -- -Z time-passes` (`RUSTC_BOOTSTRAP=1`) | 4.57 s wall, rustc 4.31 | 2.95 s wall, bin's rustc 1.15 | 2.95, 2.98, 2.85 | 1.60, 1.55, 1.55 |
| 3b. the edited crate's test build | `cargo rustc --profile test -p wsp-engine --lib -- -Z time-passes` | (was the bin) | 3.54 s wall, rustc 3.29 | 3.54, 3.56, 3.44 | 1.70, 1.70, 1.64 |
| 4. the affected check after the edit | `wsp check --affected --run` | 242.53 s (`cargo test -p wsp`) | **162 s** (`wsp-engine` 138 to 275 s, `wsp-trees` 18 s) | 157, 162, 294 | 0.79, 2.20, 0.62 |
| 5. the full suite | `cargo test --workspace --no-fail-fast` | 332.58 s | 334.08 s | 334.08, 335.68, 333.88 | 1.49, 1.72, 1.34 |

Once: a cold `cargo test --workspace --no-run` took 60.99 s at load 0.85 (before: 61.93 s at 3.41). The suite lists
**4147** tests (before: 4102).

- **rustc's passes** (run 2), engine test build / bin test build / the old bin: front end (expand, resolve, type
  check, borrow check, checks) 1.10 / about 0.26 / 1.56 s; codegen (mono, partition, IR, LLVM) 0.66 / about 0.13 /
  0.92 s; dep graph 0.44 / 0.13 / 0.64 s; link 0.41 / 0.42 / 0.50 s; total 3.29 / 1.15 / 4.31 s.
- **Step 4** now leaves `cargo test -p wsp` to the barrier (wsp-433, wsp-522): the plan is `crate:wsp-engine` and
  `crate:wsp-trees`. Run 3 took 294 s against 157 and 162. Its package throttle count rose by 1,327 (below); the cause
  was not established.
- **Step 5:** the `wsp-engine` unit tests are now the long binary, 137 to 139 s (about 42 % of the wall); the bin's
  unit tests are 6.3 s. Then `server` 41 s, `wip_api` 25 s, `standalone` 20 s, `wsp-trees` 17 s. Run 2 failed two
  tests in `wsp-engine` (`mirror::tests::a_pass_where_only_machines_json_moved_reads_the_machines_alone` and
  `mirror::tests::diff_edit_removal_and_quiet`).
- **Thermal: the package did throttle.** `cpu0/thermal_throttle/package_throttle_count`, read before and after each
  run, did not move or moved by 3 at most in steps 1 to 3. It rose by 663 in the cold build, 1,327 in step 4's run 3
  and 312 to 985 in each full suite. The package sensor read up to 95 °C at a run's start. The counter says that
  throttling happened, not what it cost. wsp-429 and wsp-430 did not read it.

### The stage split

**Only one wsp row, wsp-522, had landed after wsp-440** when this was measured, so there are no after-phase-2 medians.
The split on 10 or more such rows is **wsp-524**. The middle column below is 26 rows landed after wsp-361's fix
(2026-11-24 09:15Z). **Most of them landed before the cut** (all but wsp-440 and wsp-522), and the read-api rows are
also in wsp-429's 101. It answers cut 2, not phase 2.

Minutes, median / p75, unless marked s. Method as in "How the stages were cut".

| Stage | Before (wsp-429, 101 rows) | Since wsp-361's fix (26 rows) | After wsp-440 (wsp-522, n=1) |
|---|---|---|---|
| load1 at the row checks' start | 2.3 / 4.7 | 1.6 / 2.9 | 0.5 |
| start → landed, held taken out | 9.5 / 14.1 | 12.6 / 16.4 | 6.7 |
| seat start | 6 / 7 s | 6 / 8 s | 7 s |
| **the agent's own work** | **6.3 / 10.1** | **7.1 / 11.2** | **5.1** |
| `wsp check`, the row's wall | 0.7 / 1.0 | 0.4 / 1.4 | 0.3 |
| one check run (`took`) | 30 / 50 s | 25 / 54 s | 19 s |
| verifier, all of it | 1.3 / 2.2 | 1.6 / 2.6 | 1.0 |
| &nbsp;&nbsp;its checks | 0.1 / 0.5 | 0.1 / 0.3 | 0.0 |
| rebase and land | 2 / 6 s | 2 / 5 s | 5 s |
| other waits | 0.0 / 0.3 | 0.0 / 0.8 | 0.0 |
| barrier: the row's share | 1.4 / 5.4 | 3.0 / 9.3 | 0.4 |
| group barrier wall, per group | 3.4 / 7.0 | 3.7 / 16.0 (16 groups) | 0.4 |
| barrier's own run, where a step ran | 44 / 72 s | 41 / 45 s (9 runs) | 25 s |
| check seat's by-hand builds and tests, per seat | 73 / 134 s | 24 / 403 s | no seat |
| the same per row (÷ members) | 0.35 / 1.56 (0-step stretch) | 0.2 / 3.7 (mean 1.7) | 0 |

- **The stage with the most minutes per row is the agent's own work** (7.1 of 12.6 min; 5.1 of 6.7 on wsp-522). Next at
  the median are the barrier share (3.0) and the verifier (1.6). The barrier share's mean, 20.7 min, is mostly holds:
  turnaround group 2 waited 593 min for the person's go.
- **Cut 1 (verifier beside the check):** the shorter of check and verifier is 0.4 / 1.3 min per row (mean 0.8) on the
  26 rows.
- **Cut 2 is only partly confirmed.** The barrier's own run no longer reads 0 steps (16 runs, none). Check seats still
  ran the full suite by hand in 3 of 16 groups: read-api 5 (328 s), turnaround 3 (329 s) and turnaround 4 (378 s).
  6 of 16 had more than a minute of by-hand builds and tests.
- **Cut 10 (a barrier without a check seat)** happened once: turnaround group 6 was "passed by wsp with no agent".
- **Send-backs:** 11 of 26 rows, 23 send-backs (14 verifier, 9 check); rework per row 0.0 / 1.7 min (mean 3.5).

Left out of the 35 wsp rows landed since the fix: four retro rows, omarchy-env's wsp-427 (its group ended the 0-step
stretch) and the docs-only turnaround rows wsp-429 to wsp-432. The scripts and outputs are in
`~/.local/state/wsp/scratch/wsp-442/`: `loop/` (`run.sh`, `results.tsv`, each run's log) and `split/` (`collect.py`,
`analyze.py`, `post.py`, `rows.json`, `stages.json`).

### The re-rank

Written by wsp-442 (leg rank) on 2026-11-25, from the two sections above and figures it added the same day at load1
0.2 to 3.1 on the Linux host, the tree at d1521b0. Its scripts and outputs are in `~/.local/state/wsp/scratch/wsp-442/rank/`:
`run.sh` and `results.tsv` (the surface, timeline and feature-switch timings, and `--report-time` over the engine's
unit tests), `barrier.py` (the barrier share split per row), `long26.py` and `pp.py` (the agent seats' cargo calls on
the 26 rows), and the per-process runs' test lists.

**The rule.** The minutes per row are taken over the 26 rows landed since wsp-361's fix. Each is shown as median / p75 (mean).
- **The mean is the expected gain**, and it decides: a cut at a mean of a minute or more gets a row.
- The two groups that stood over an hour, turnaround 2 (593 min waiting for the person's go) and read-api 3 (120 min,
  held), are left out of the barrier figures where marked. No cut here removes a person's wait.

**The finding.** The engine's unit-test binary does not run its tests in parallel.
- Every test that touches a store or a backend holds `util::ENV` (`crates/base/src/util.rs`, `isolated`), one lock for
  the whole process, so libtest's 32 threads queue on it.
- `cycle::` (186 tests) takes 11.85 s on 32 threads and 11.85 s on one thread. Run alone, its slowest tests take 0.00 to 0.34 s.
  Under `--report-time` in a full run, the slowest tests read 40 to 70 s each: that is time spent queued.
- One process per test, 32 at a time, is what `cargo nextest run` does (`.config/nextest.toml` has been in the repo since wsp-434).
  Run that way:
  - `cycle::` takes 3.78 s (twice);
  - the whole `wsp-engine` unit binary takes 25.2 and 25.5 s, against 133 s in-process (2293 tests, no failures);
  - `wsp-trees`' unit tests take 4.2 s against 18.8 s.
- That is the test-binary cut that is left after phase 2. It is not sharding the binary and not splitting it: the time
  is lost inside one binary, to its lock.

| # | Cut | Min per row, median / p75 (mean) | The figure it rests on | Call |
|---|---|---|---|---|
| A | Tests one process each (nextest), so store tests stop queueing on `util::ENV` | agent seats alone 0.67 / 1.07 (1.09); about 1.5 at the mean with the suites and checks | Per-process: `cycle::` 11.85 to 3.78 s, engine binary 133 to 25 s, trees 18.8 to 4.2 s. The seats' own test runs are 1.00 / 1.60 (1.64) min per row | **filed: wsp-527** |
| 1 | The verifier beside the check | 0.4 / 1.3 (0.8) | min(check, verifier) on the 26 rows | already a row, wsp-508. Below a minute |
| 10 | A passing barrier without its check seat | 0.19 / 0.93 (0.78); 0.36 / 2.32 (1.25) if the seat's by-hand suite is dropped, not moved | The seat's share less the barrier's run, on the 11 groups whose seat said "passes" | not filed |
| 6 | Shard or split the test binary (binaries side by side, or the bin's unit tests apart) | ≤ 0 / 0.8 (≤ 0.85) | Full runs only: by-hand suites 0 / 1.38 (0.88), seats' `--workspace` runs (0.59 mean), each at most 58 % shorter | not filed. A takes most of it |
| 2 | The check seat stops running the suite by hand | 0 / 1.38 (0.88) of time spent, not saved | Full suite by hand in 3 of 16 groups; read-api 1 ran it beside 32 busy loops | not filed. wsp-517 decides where the suite runs |
| 9 | Groups beside a held barrier | ≤ 3.0 / 9.3 (≤ 20.7) | The whole barrier share; its mean is the two long holds | already a row, wsp-443. Needs Ed |
| P2 | Phase 2 as cut (for reference) | ≤ 0.15 / 0.33 | Engine edit 4.47 to 3.45 s, × 9 / 20 builds | done |
| 8 | Limit send-backs (wsp-386) | 0 / 0 (≤ 0.15) | Before the fix. Not recounted: the 26 rows' send-backs carry no reason in `split/` | already a row, wsp-386 |
| 5 | Thermal and load | 0 at the median; thermal not established | 14 of 109 bin-building row steps at load ≥ 4, median 1 s at both | not filed |
| S | `wsp-surface` | ≈ 0 | A surface edit already builds in 1.35 s (`--bin wsp`) and 2.15 s (`cargo test --no-run`) | not filed. Its gate is met with no cut |
| T | `wsp-timeline` | ≈ 0 | A timeline edit: 1.35 s and 2.4 s, the same as surface | not filed |
| 7 | One feature set across cargo commands | 0 | A switch now costs 0.24 s each way (was 34.8 s) | not filed. It is gone |

The savings do not add. A shortens what 6 and 2 would act on, and 10 overlaps 2.

**Why each other cut was not filed:**
- **1, the verifier beside the check.** It saves 0.4 / 1.3 min per row (mean 0.8), below a minute. It is already filed
  (wsp-508), so nothing new is filed.
  - **Does the verifier rebuild what the check built?** No. Its own cargo calls take about 1 s, because they run in the
    row's warm tree. All its checks together are 0.1 / 0.3 min (mean 0.2), so reusing the check's greens would save at
    most that.
  - Its time is reading the row: 1.3 / 2.2 min.
- **10, a barrier without a check seat.**
  - Where the seat said "passes" (11 of 16 groups), its share less the barrier's own run is 0.36 / 2.32 (1.25). Most of
    that is its by-hand full suite.
  - The barrier's run does not run the suite, although its line says "scope: full" (wsp-517). Dropping the seat would
    drop the only full run, not save it. With the suite kept somewhere, the cut is 0.19 / 0.93 (0.78).
  - It is also the person's call: the seat is there to catch members that undid each other. In 5 of the 16 groups it
    filed a follow-up or held the group.
- **6, sharding or splitting the binary.** It only shortens full runs, the suite or a whole crate.
  - By hand: the check seats' suites (0.88 mean) and the seats' own `--workspace` runs (0.59 mean). Run side by side,
    the suite's binaries would fall from 334 s toward the engine binary's 138 s, at most 58 % shorter, so ≤ 0.85 at the
    mean and 0 at the median.
  - A takes the engine binary itself from 133 to 25 s and runs binaries side by side too. What 6 would add after A is
    small, so it is folded into A.
- **2, the barrier's own run.** It works again: none of the 16 runs read 0 steps.
  - The check seats' by-hand suites are 0 / 1.38 (0.88) per row. They are not waste while the run leaves the suite out
    (wsp-517), so this is time spent, not a saving.
  - wsp-517 is already filed and decides where the suite runs. A shortens the suite wherever it runs.
- **9, groups beside a held barrier.** It is already filed (wsp-443) and waits on Ed. Its bound is the whole barrier
  share, 3.0 / 9.3 (20.7), and the mean is two holds. One of them was the person's go, which it would not remove on
  turnaround, whose groups are serial.
- **5, thermal and load.**
  - **Load:** the row checks' load fell to 1.6 / 2.9 (from 2.3 / 4.7). The bin-building row steps take a median 1 s
    whether the load is under 4 or not (95 and 14 steps), because they mostly reuse a warm build. Holding them saves nothing.
  - **Thermal:** the package throttle counter rose in every long run (the cold build, the three suites and one check run,
    294 s against 157 and 162). The three suites throttled and still agreed within 2 s, so what throttling costs is not
    established. wsp reads the clock only from `pmset`, so the throttle's check cap does nothing on the Linux host.
  - A shortens exactly the long runs that heat the package.
  - Ed's Mac (96 s per check run) can only be measured there.
- **S and T, `wsp-surface` and `wsp-timeline`.** Measured today, three runs each after a one-constant edit:
  - in `src/arrange.rs`: 1.44, 1.34, 1.35 s for `cargo test --bin wsp --no-run`, and 2.25, 2.15, 2.14 s for `cargo test --no-run`;
  - in `src/timeline.rs`: 1.35 s each, and 2.34, 2.45, 2.45 s.

  The surface gate (`cargo test --no-run` under half the bin crate's 4.47 s, so under 2.2 s) is met by the bin crate as
  it is, now that engine is out. A crate of its own could take at most part of the bin's 1.15 s of rustc. Surface was
  touched by 2 of 81 commits and timeline by 10, so the gain per row rounds to 0. The gate has nothing left to decide.
- **7, one feature set.** After a warm `--workspace` build, `--bin wsp --no-run` took 0.24 s, and so did the way back,
  twice. `-p wsp-engine` was the same after one catch-up build of 3.55 s. The 34.8 s wsp-430 saw is gone at df704d8.
- **8, limit send-backs.** It is already filed (wsp-386) at ≤ 0.15 at the mean.

**What wsp-524 could change.** wsp-524 is the stage split on 10 rows landed after wsp-440. It recounts the stages
from the rows' logs and transcripts, so it can move the ranks that rest on the 26 rows:
- **A:** the seats' test-run minutes. After the cut, seats run `-p wsp-engine <module>::`, so more of their runs queue
  on the lock, and A would rise.
- **1:** the check and the verifier. The check's wall was 0.3 min on wsp-522 against 0.4. A smaller check makes 1 smaller.
- **10 and 2:** the barrier share and the by-hand suites. They are 3 of 16 groups today, and wsp-517 would change both.

It cannot move S, T, 7 or 5's load figure. Those come from the timings above, not from the rows.

### Cut A, measured (wsp-527)

Written by wsp-527 on 2026-11-25. A check step whose command is one plain `cargo test` that names its targets
(`--lib`, `--bins`, `--bin`, `--tests` or `--test`) now runs as `cargo nextest run` with the same package, targets and
filters (and `--no-tests=pass`, so a filter that matches nothing still passes, as it does under `cargo test`), wherever
the host that runs the job answers `cargo nextest --version`. Where it does not answer, the step runs as written. The
job's first line says which runner ran, and the step's line repeats it:
`✓ unit · cargo test -p wsp-engine --lib cycle:: · by cargo nextest run · 3s`. The step's command is unchanged, and so
is its green key, so a green from either runner counts for the other at the same inputs. `crates/engine/src/check.rs`
(`by_nextest`, `runner_script`, `via_of`) does this, and the barrier, the verifier's check and a row's check all go
through it.

A command with no target selection (`cargo test`, `cargo test --workspace`, `cargo test -p <crate> <filter>`) runs
doctests, and nextest runs none, so it stays on `cargo test` and keeps the one lock per process. Following nextest with
`cargo test --doc` would not be the same: an explicit `--doc` runs the doc blocks of a `doctest = false` crate, and fails
on a package with no library. A step takes nextest by naming its targets (`--tests` for all but the doctests and the
example builds). Only flags known to read alike are passed on, with cargo's `--profile`, `-j` and `-m` written as
nextest's `--cargo-profile`, `--build-jobs` and `--manifest-path`; any other flag, pipe, `$` or quote runs as written.
`wsp verify`'s own cargo runs read libtest's report and stay on `cargo test`: follow-up wsp-536.

cargo-nextest 0.9.152 is installed on the Linux host now, so the person had nothing to install.

**Timings.** The Linux host, the tree at c0c669b, every target built before the first run. Three runs each, before
(`cargo test`) and after (`cargo nextest run`), alternating. Each run went through the seat's cargo shim, as a seat's
run does (a manager job, `nice -n 12`, `CARGO_BUILD_JOBS=3`). Load1 was 0.69 to 3.39 at each start. Script and logs:
`~/.local/state/wsp/scratch/wsp-527/measure.sh`, `times`.

| What | cargo test (s) | cargo nextest run (s) | Done-when |
|---|---|---|---|
| `-p wsp-engine --lib cycle::` (186 tests) | 12.08, 11.96, 11.98 | 4.07, 4.07, 4.08 | ≤ 4.5 s: held |
| `-p wsp-engine --lib` (2299 tests) | 138.1, 138.1, 133.5 | 25.7, 25.8, 25.5 | ≤ 30 s: held |
| `--workspace` (4143 run, 12 ignored) | 326.5, 331.9, 330.4 | 53.8, 53.7, 54.6 | from 334 s |

Every nextest run passed. The second `cargo test` run of the engine binary exited 101. The third run overwrote its log,
so the failing test is not named; it passed in the other two runs. The `--workspace` nextest runs reported 2 tests
slow (over 30 s) and none terminated.

**The count.** `cargo nextest list --workspace` lists 4152 tests at 287f379, the same 4152 that `cargo test --workspace
-- --list` lists, 12 of them ignored. With this row's three new tests, both runners count 4155. Doctests: nextest runs none. A check step that would run them under
`cargo test` is left on `cargo test` (above), so no step loses one.
