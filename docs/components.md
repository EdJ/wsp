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
modules added since the spike placed. A new module goes in this table or the test fails. Every upward use the code
has today is listed in `docs/components-upward.txt`; the test fails on one not listed and on a listed one the code
no longer has, and that file may only lose lines. `cargo test --test layering -- --nocapture` prints the per-pair count.

Measured by it on 6 Nov (`ad10223`), with nothing moved yet: **171 upward edges, 335 symbols** (the spike gives the same 171
with this map; the 80 / 207 of try 3 assume `p1` and `p2` have moved their symbols), and **890 distinct
cross-component symbols over 60 of the 72 pairs** (the spike gives 890 too; the 499 / 49 below it does not reproduce).

| component | modules |
|---|---|
| `base` | `base`, `util`, `calls`, `fm`, `input`, `risk`, `memory`, `choices` |
| `store` | `model`, `checks`, `caption`, `store`, `resolve`, `message`, `lapse`, `sharing`, `cmd_migrate`, `testkit`, `governors`, `agent_kind`, `verbs`, `decisions`, `log`, `tags` |
| `wire` | `herdr`, `place`, `place_herdr`, `place_compound`, `place_super`, `artefact`, `cmd_mandate`, `tunnel`, `far`, `launchd`, `detect_override`, `fake` |
| `trees` | `cmd_checkout`, `cmd_verify`, `treecopy`, `check_tree`, `throttle`, `warm`, `overlap`, `guard`, `greens`, `at_stop`, `gc` |
| `engine` | `stage`, `history`, `after`, `worklist`, `cycle`, `barrier`, `barrier_run`, `barrier_replay`, `verification`, `check`, `leg_tree`, `cold`, `affected`, `build_output`, `did`, `closing`, `ending`, `repair`, `split`, `size_check`, `starts`, `stand_down`, `untaken`, `waiting`, `legs`, `pieces`, `piece_seat`, `piece_close`, `inner`, `leg_seat`, `cmd_worklist`, `cmd_task`, `packet`, `packet_gen`, `verifier_packet`, `attention`, `kind_attention`, `cmd_attend`, `cmd_message`, `hold`, `hold_close`, `pickup`, `waits`, `scope`, `retro_yours`, `address`, `tell_log`, `scratch`, `disk` |
| `seats` | `agent_commands`, `kind_data`, `cmd_project`, `cmd_spawn`, `cmd_agent`, `cmd_resume`, `wake`, `room`, `cmd_govern`, `cmd_machine`, `cmd_sandbox`, `cmd_watch`, `cmd_brief`, `cmd_install`, `cmd_role`, `cmd_stamp`, `stuck`, `sync` |
| `timeline` | `timeline`, `cmd_cost`, `cmd_burn`, `cmd_calls`, `cmd_report`, `wallbuckets`, `cmd_attempts`, `cmd_verifications`, `retro`, `retro_run`, `retro_series` |
| `surface` | `panel`, `detail`, `draw`, `kanban`, `live`, `story`, `arrange` |
| `top` | `main`, `registry`, `readonly`, `daemon`, `leaked_agents`, `cmd_piece` |

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
| `wsp` (root) | `src/` | — | every module not yet cut; the binary, `build.rs`, the stamp (`src/base/stamp.rs`) | this file |
| `wsp-base` | `crates/base` | `c1` (wsp-408) | `util`, `fm`, `input`, `risk`, `choices`, `base::{args, git, labels, seat, term}` | `crates/base/MAP.md` |
| `wsp-testkit` | `crates/testkit` | `c1` (wsp-408) | the git repository fixture (`repo`, `repo_at`, `git`); the store-backed half is in `crates/store/src/testkit.rs` (a `wsp-testkit` that depended on `wsp-store` would give the store's own unit tests a second `Store`) | its `lib.rs` |
| `wsp-store` | `crates/store` | `c5` (wsp-412) | `model`, `store`, `resolve`, `message`, `lapse`, `sharing`, `checks`, `caption`, `governors`, `agent_kind`, `log`, `decisions`, `tags`, `verbs`; the store-backed test kit | `crates/store/MAP.md` |
| `wsp-store-contract` | `crates/store-contract` | `c5` (wsp-412) | the constants the records name (`SCHEMA`, the section lists, `DEFAULT_LEASE`, the policy words); the types stay in `wsp-store` while they carry inherent methods | its `lib.rs` |

`disk` and `memory` are mapped to `base` in the layering table but stay in the root package: each reaches up
(`cmd_checkout`, `place`). Dependencies are `serde_json` only; the members are path dependencies in the workspace.
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

| crate | its contract (what others may use) | one-writer facts (only this crate…) |
|---|---|---|
| `wsp-base` | `Args`, `Stamp`, `git(…)`, `Clock`, `Colour` | …owns no state. Anything stateful is refused here |
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
logic edit. The check on every cut: `cargo test -- --list` counts the same tests before and after (2534 today).
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
| `c1` | workspace skeleton + `wsp-base` (+ `wsp-testkit`). `wsp verify`, declared `## Checks`, `affected`, `packet_gen` taught `-p` | install the result; edit `wsp-base` once and time it; the 2534 count |
| `c2` | `wsp-store`, after its seams (wsp-460) | edit it once and time it; the count of tests |
| `c3` | `wsp-wire`, after its seams | |
| `c4` | `wsp-trees`, after its seams | |

After phase 2 (`engine` and `seats` cut): `wsp-surface` and `wsp-timeline`. The **surface build gate** (formerly `c2`) moves there: edit `wsp-surface`, time `cargo test --no-run`, **under half the 21–32s, else stop**. It is measurable only once `engine` and `seats` are cut, because until then nothing below `surface` is a crate to rebuild around.

**Phase 2, gated:** `engine` and `seats` (63k lines, 50% of the code) are cut only after `p5` has held for a
fortnight and the contract-change share is known. If the share is above ~35% the gain is under 40% and the cut is not
worth the churn, and the plan stops at six crates.

Hazards the order is built around: the cut rows touch every file in flight (rename detection is the only protection,
hence pure moves and `git mv`); `tests/`-style paths in checks (`wsp check` scripts in each project's `## Checks`,
and the `affected` key) must name the new paths in the same row; and **`serde_json` stays the only external
dependency**, a workspace's members are not dependencies in the sense of that rule, and the commit should say so.
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
