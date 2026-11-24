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
