# Where the tests are, and which can move

Row `wsp-489`, 2026-11-08, at `93025a9`. An audit: it measures and classifies, and **moves nothing**: no code, no
test moved, no barrier changed. The plan that decides what moves, when, and the tiers is `wsp-483` (state-core).
Ed, 2026-11-07: *"we were meant to attack this a bit more with the components worklist, since wsp was highly
concentrated before, perhaps we should start with a pass to move any tests that aren't allocated nicely, then add
tiering etc. after. We should identify if this is a before or after redesign problem from state-core."* and then
*"let's just file an audit row in state-core instead of doing this ourselves"*.

## 1. The answer

**It is an after-the-redesign problem.** Of the root crate's 2,209 tests in 94 files, only **19 tests could move
today** to a crate that exists: 2 whole files (`artefact`, `cmd_mandate`, 11 tests, to `wsp-wire`) and 8 pure
`Args::parse` tests inside `main.rs` (to `wsp-base`). Together they take 2.1 s of the root's 1,265.0 s.
**1,996 tests in 89 files (90%, 1,241.3 s, 98% of the root's time) are tests of modules that have not been cut
yet**, and they move with their modules when state-core cuts them. 194 tests are end-to-end tests of a whole
(the panel storyboard, a sandboxed wsp, the verb table) and have no module to follow. No test was found in the wrong
place: the subjects that already have a crate (store, wire, trees, base) took their tests with them when those
crates were cut, so a "move what is not allocated nicely" pass has almost nothing to do before the redesign.

| class | files | tests | seconds | of which ≥ 1 s |
|---|---|---|---|---|
| (a) could move now, to a crate that exists | 2 whole files, plus 8 tests of `main.rs` | 11 + 8 = 19 | 2.1 | 0 |
| (b) end-to-end across components, stays | 3 files (`main.rs` for its other 29 tests) | 194 | 21.6 | 5 tests, 6.6 s |
| (c) stays until state-core cuts its module | 89 | 1,996 | 1,241.3 | 399 tests, 861.7 s |
| **root crate** | **94** | **2,209** | **1,265.0** | **404 tests, 868.3 s** |

**Where the time is.** The whole workspace's tests add up to 1,724.0 s. The root crate is 73% of it (1,265 s). Inside
the root, the engine's 38 files hold 933 tests and 958 s, **56% of all test time in the workspace**. The rest: `wsp-trees`
(already a crate) 19%, seats 14% (the 17 files of (c) plus `cmd_sandbox`), `wsp-wire` 4%, `wsp-store` 3%, and surface,
timeline, base and the binary's own layer **under 4% together**. Cutting those last four out of the root would not
move test time. What decides how long a barrier is are the 336 tests of one second or more that live in the engine's
files (§5).

**What tiering can save, priced** (§5). A **fast lane of the tests under 1 s** is 2,313 tests and 459.6 s, about
**58 s wall** here against 218.6 s for the whole run. The 528 tests at or over 1 s are **73% of the time and 19%
of the tests**. Which lane a test belongs to is a property of its measured time, not of its file, and the file class
does not predict it (§5).

## 2. How it was measured

- One `cargo nextest run --workspace --no-fail-fast` at `93025a9` (`cargo-nextest 0.9.143`), JUnit output
  (`store-success-output = false`) from a config file kept outside the tree, so `.config/nextest.toml` is as it was.
  8 cores (`hw.ncpu`), nextest's default of one test process per core. The machine was not idle: load averages 4–8
  from other agents during the run, and it throttles under load (`machine-is-thermally-bound`). Read the seconds as a
  ranking and an order of magnitude; one run is good to perhaps ±30%.
- **Wall 218.6 s** for the test phase (nextest's `Summary`), 348 s from the command start including the compile.
  The sum of the per-test times is 1,724.0 s, so the run kept **7.9** tests going at once on average. A test's
  time here is **its time in that contention**, not what it costs alone.
- 2,841 tests ran: 2,209 in the root binary, 217 `wsp-trees`, 181 `wsp-wire`, 169 `wsp-store`, 63 `wsp-base`, 2 in
  `tests/layering.rs`. 8 `#[ignore]`d tests did not run. **1 failed**: `wsp-wire`
  `place_compound::tests::a_delivery_that_outlasts_its_wait_is_asked_again_under_the_same_id`
  (`place_compound.rs:2266`, `fs::read_to_string(&asked)` returned `NotFound`: it waits 2.5 s for a stub supervisor to
  write a file, and the machine was loaded). It passes alone (2.9 s, rerun). **4 leaked** (a child outlived the
  test); not looked into.
- Test to file: nextest names a root test `module::tests::name`, mapped to `src/module.rs` or `src/dir/file.rs`. All
  2,209 map, and the per-file counts equal `grep -c '#[test]'`. The unit of the classification is the **file**: a root
  test lives in a `#[cfg(test)]` module at the foot of the file it tests, so it cannot move without its module (§6).
  Every test's name, time and status from the full run is kept in the row's scratch (`tests.json`, with nextest's `nextest.out`).
- The module → component map is the layering table in `docs/components.md`; the upward-edge counts are the lines of
  `docs/components-upward.txt` that start with the module.

## 3. Time per crate

| crate | tests | seconds | share | slowest test |
|---|---|---|---|---|
| wsp (root bin) | 2,209 | 1,265.0 | 73% | 19.1 s |
| wsp-trees | 217 | 328.2 | 19% | 18.4 s |
| wsp-wire | 181 | 74.2 | 4% | 5.2 s |
| wsp-store | 169 | 50.2 | 3% | 12.8 s |
| wsp-base | 63 | 5.5 | 0% | 2.1 s |
| wsp tests/layering.rs | 2 | 0.9 | 0% | 0.6 s |
| **workspace** | **2,841** | **1,724.0** | | |

Spread of the per-test time (all 2,841):

| per-test time | tests | seconds | share of seconds |
|---|---|---|---|
| 10 s and over | 10 | 132.5 | 8% |
| 5 to 10 s | 19 | 122.8 | 7% |
| 2 to 5 s | 195 | 586.3 | 34% |
| 1 to 2 s | 304 | 422.7 | 25% |
| 0.5 to 1 s | 367 | 267.9 | 16% |
| 0.1 to 0.5 s | 407 | 111.3 | 6% |
| under 0.1 s | 1,539 | 80.5 | 5% |

Median test: 0.083 s (which includes nextest's process start). 1,539 tests (54%) take under 0.1 s and add up to 80 s; 29 tests (1%) take 5 s or more and add up to 255 s.

Time per module inside the crates, the ten largest (`wsp-trees` is 217 tests and 328 s, and 238 s of that is two modules):

| crate | module | tests | seconds | slowest |
|---|---|---|---|---|
| wsp-trees | `cmd_checkout` | 43 | 122.0 | 5.1 s |
| wsp-trees | `treecopy` | 23 | 116.6 | 18.4 s |
| wsp-wire | `place_compound` | 31 | 41.8 | 5.2 s |
| wsp-trees | `throttle` | 47 | 36.0 | 4.4 s |
| wsp-store | `store` | 42 | 18.0 | 2.8 s |
| wsp-store | `lapse` | 6 | 17.9 | 12.8 s |
| wsp-trees | `check_tree` | 20 | 14.1 | 1.8 s |
| wsp-wire | `far` | 19 | 13.3 | 4.5 s |
| wsp-trees | `at_stop` | 9 | 11.1 | 2.9 s |
| wsp-trees | `guard` | 21 | 10.7 | 2.9 s |

Time per root file is the `s` column of §7; the ten largest are in §5.

## 4. The slowest 50 tests

Of the workspace's 2,841. Together 349.7 s, 20% of the time; the first ten are 132.5 s.

| # | s | crate | test |
|---|---|---|---|
| 1 | 19.1 | wsp | `cmd_spawn::a_spawn_that_leaves_the_order_typed_is_owed_a_submit_not_a_flag` |
| 2 | 18.4 | wsp-trees | `treecopy::a_verb_that_ran_and_failed_keeps_its_own_status` |
| 3 | 14.7 | wsp | `barrier_run::a_run_that_waits_for_the_tree_lock_says_for_whom_and_does_not_count_the_wait` |
| 4 | 12.8 | wsp-store | `lapse::every_transition_closes_what_it_took_away_and_nothing_else` |
| 5 | 12.6 | wsp-trees | `treecopy::the_agents_commits_come_back_as_patches_with_their_authors_and_messages` |
| 6 | 12.3 | wsp | `cycle::a_trunk_that_moves_during_the_pass_reruns_only_the_affected_checks` |
| 7 | 11.5 | wsp-trees | `treecopy::the_shim_marks_tree_verbs_and_only_those_and_refuses_checkout_and_land` |
| 8 | 11.1 | wsp | `cmd_worklist::a_shut_barrier_names_which_members_touched_one_file_while_it_stands_shut` |
| 9 | 10.1 | wsp | `cycle::a_conflict_after_the_trunk_moved_blocks_a_ready_member` |
| 10 | 10.0 | wsp-trees | `treecopy::copy_back_brings_only_what_differs_and_deletions_and_then_nothing` |
| 11 | 8.9 | wsp-trees | `treecopy::copy_up_matches_by_hash_and_records_where_it_stands` |
| 12 | 8.0 | wsp | `repair::a_run_whose_member_agent_is_killed_mid_group_reaches_its_barrier_without_a_verb` |
| 13 | 8.0 | wsp | `worklist::the_record_names_the_commit_and_files_each_member_put_on_the_trunk` |
| 14 | 7.9 | wsp | `cycle::a_rebased_land_is_not_verified_again` |
| 15 | 7.6 | wsp | `verification::a_trunk_that_keeps_moving_blocks_after_three_rounds` |
| 16 | 7.5 | wsp | `cmd_spawn::a_spawn_whose_first_order_is_dropped_sends_it_again_and_records_what_is_still_owed` |
| 17 | 7.3 | wsp | `cycle::a_block_on_a_ready_member_leaves_the_trunk_as_it_was` |
| 18 | 6.8 | wsp | `ending::a_release_and_an_attempts_end_leave_the_store_committed` |
| 19 | 6.8 | wsp | `cycle::a_ready_member_is_verified_in_its_tree_and_a_hold_lands_it` |
| 20 | 6.7 | wsp | `cycle::a_red_rerun_after_the_trunk_moved_blocks_a_ready_member` |
| 21 | 5.4 | wsp | `repair::a_barrier_held_by_a_blocked_verifier_names_the_block` |
| 22 | 5.4 | wsp | `hold_close::a_turn_that_ends_a_prompt_or_a_session_abandons_what_is_open` |
| 23 | 5.4 | wsp | `cmd_message::a_wait_prints_the_answer_as_the_agent_reads_it` |
| 24 | 5.3 | wsp | `leg_tree::a_conflicting_pair_leaves_the_rows_tree_clean_at_a_commit_and_names_the_branch_and_files` |
| 25 | 5.2 | wsp-wire | `place_compound::detected_state_for_a_pi_seat_reads_the_manifest_not_the_truth` |
| 26 | 5.2 | wsp-wire | `place_compound::a_delivery_is_the_supervisors_answer_read_and_a_failed_names_its_reason` |
| 27 | 5.2 | wsp | `cmd_message::the_person_cancels_any_kind_of_ask_and_the_asker_is_told` |
| 28 | 5.1 | wsp-trees | `cmd_checkout::standing_in_a_repository_still_decides_which_one_gets_the_tree` |
| 29 | 5.1 | wsp | `cmd_message::a_wait_ends_as_the_question_does_and_a_timeout_leaves_it_open` |
| 30 | 5.0 | wsp | `repair::a_seat_that_is_working_blocked_starting_or_unreadable_is_not_landed_over` |
| 31 | 5.0 | wsp | `leg_tree::a_pane_standing_in_it_or_a_leg_still_going_keeps_the_tree` |
| 32 | 4.9 | wsp | `leg_tree::a_legs_tree_gets_the_projects_at_stop_in_it_before_it_goes` |
| 33 | 4.8 | wsp | `leg_tree::a_tree_with_an_untracked_file_is_kept_and_named` |
| 34 | 4.8 | wsp | `leg_tree::a_leg_tree_reads_as_its_rows_to_checkout` |
| 35 | 4.8 | wsp | `leg_tree::the_spawns_prepare_cuts_siblings_trees_and_merges_at_the_join` |
| 36 | 4.6 | wsp-trees | `cmd_verify::a_test_that_fails_alone_keeps_the_assertion_the_suite_would_have_shown` |
| 37 | 4.6 | wsp | `leg_tree::a_clean_join_merges_both_and_the_trees_go` |
| 38 | 4.5 | wsp | `cmd_spawn::a_react_rows_verifier_spawn_names_its_packet_file_and_the_file_keeps_the_diffs_lines` |
| 39 | 4.5 | wsp-wire | `far::the_trust_command_trusts_the_trees_own_path_keeps_the_rest_and_removes_it` |
| 40 | 4.5 | wsp-trees | `treecopy::the_time_is_the_first_failure_and_a_success_clears_it` |
| 41 | 4.4 | wsp-trees | `throttle::a_second_build_in_one_tree_waits_for_the_first_in_every_mode` |
| 42 | 4.3 | wsp | `leg_tree::edits_in_different_places_of_one_file_merge_without_anyone_being_told` |
| 43 | 4.3 | wsp | `cmd_worklist::ls_draws_planned_lists_in_queue_order_each_with_what_it_waits_on` |
| 44 | 4.3 | wsp | `leg_tree::a_join_holds_while_someone_stands_in_the_rows_tree_or_it_is_dirty` |
| 45 | 4.3 | wsp-trees | `cmd_checkout::the_sweep_takes_a_closed_tasks_tree_and_leaves_everything_it_cannot_prove` |
| 46 | 4.2 | wsp | `cycle::a_pass_opens_on_the_rebased_commit` |
| 47 | 4.2 | wsp | `cycle::a_rebase_that_does_not_apply_sends_the_member_back_and_spends_no_agent` |
| 48 | 4.1 | wsp-trees | `treecopy::carrying_commits_refuses_to_touch_a_path_the_seat_changed` |
| 49 | 4.1 | wsp | `cmd_install::what_the_source_carries_is_asked_of_the_source_and_not_of_its_tree` |
| 50 | 4.1 | wsp-trees | `treecopy::the_seat_holding_the_far_version_already_is_agreement` |

Not diagnosed: the audit prices by time and does not say why a test is slow (git, a process, a wait on a deadline).
The git repository fixture is already a template copy (`crates/testkit`), and a repository with a commit costs 0.1 s
here, so a `git init` per test is no longer the suspect that `docs/components.md` §3 named; whoever works on
these should read the top 50 before assuming a cause.

## 5. Tiering and a slow lane, priced

Tiers exist already (`docs/scoped-barriers.md`): the member's own `wsp verify`, the group barrier (scoped by what
the group touched), the list's last group (the full suite, always), and the periodic safety net. What they lack
is a split of the **suite itself**: in the root crate any change reruns 2,209 tests (73% of the seconds), and
`fleet-fixes-1.scope.json` (this row's brief; not re-measured here) shows a 677 s barrier that saved 0 s because no group
had a kind. A slow lane is a different cut: by the cost of the test, not by its crate.

| lane boundary | slow-lane tests | slow-lane seconds | share | fast lane tests | fast lane seconds | fast lane wall* |
|---|---|---|---|---|---|---|
| ≥ 0.5 s | 895 | 1,532.2 | 89% | 1,946 | 191.8 | ~24 s |
| ≥ 1 s | 528 | 1,264.4 | 73% | 2,313 | 459.6 | ~58 s |
| ≥ 2 s | 224 | 841.7 | 49% | 2,617 | 882.3 | ~112 s |
| ≥ 5 s | 29 | 255.4 | 15% | 2,812 | 1,468.6 | ~186 s |

\* seconds ÷ 7.9, the concurrency this run kept: an estimate, since a lane run alone has a different mix and a tail. The whole run was 218.6 s.

- **Where the slow lane lives.** Of the 528 tests at or over 1 s, **404 are in the root** (868 s) and 124 in the crates
  (`wsp-trees` 97, `wsp-wire` 19, `wsp-store` 7, `wsp-base` 1). In the root, **336 of the 404 are in the engine's files** (742 s). Only 37 of 94 root files hold any;
  the other 57 files hold 673 tests in 71 s and are all fast lane.
- **The ten files that hold the time:** `cycle` 205 s, `cmd_worklist` 139, `repair` 115, `cmd_spawn` 91, `worklist` 64,
  `leg_tree` 58, `cmd_message` 46, `ending` 45, `cmd_brief` 37, `barrier_run` 34: **66% of the root's seconds.** These are
  engine and seats tests that build a repository and step the run, so they are end-to-end in kind even though their
  file class says (c). The class (b) files are fast (`story` 12 s, `cmd_sandbox` 6 s, `main` 4 s): **the file class does not
  find the end-to-end tests; the measured time does.** A slow lane should be keyed by time (a threshold re-read from the
  last run) or by an attribute on the test, not by file.
- **Price of the split, at 1 s.** A group barrier that ran the fast lane only would run ~459.6 test-seconds (~58 s wall here)
  instead of ~1,724.0 (~219 s): roughly **161 s wall saved per group barrier**. The slow lane then runs at the list's last
  group, which already runs the full suite, so the end of a list costs what it does today and every barrier before it costs about
  a quarter of today's test phase. If the 677 s barrier above was all test time its fast lane is ~180 s; what share of the 677 s
  was not tests is not measured here. The cost: a bug whose only red test is slow is found a group later than today.
- **What the crate cut adds.** Scoping narrows a barrier only when a change leaves a component's dependents alone.
  Test seconds by component today: engine 958, trees 328, seats 242, wire 74, store 50, timeline 29, surface 27, base 6, top 5.
  After the cut a change that touches only seats reruns at most those 242 s (14%) rather than the root's 1,265 s; a change to the
  engine's crates still reruns the engine's 56% and whatever depends on it. `components.md` §4 counts 55 of 81 recent commits
  touching the engine and 34 seats. So **the slow lane is the lever for engine changes, the crate cut for the rest**; they multiply and
  neither needs the other.
- **Not priced:** a fixture fix for the slow tests (no cause was read), nextest's per-process start (inside every number above;
  the median test is 0.083 s), and build time.

## 6. What can move now

**(a) whole files: 2, 11 tests.** `artefact.rs` (6 tests, `wsp artefact`, `wire` in the layering table) and `cmd_mandate.rs`
(5 tests, `wire`). Each names only `util`, `base::args`, `place`, `herdr`, `resolve`, `store`, `model` and `verbs`, all of
which are already in `wsp-base`, `wsp-store` or `wsp-wire`. A move is `git mv` and the `VERBS` re-pointed; destination
`crates/wire`. Together 1.5 s, so it saves no time; it only brings the files into line with the layering table.

**(a) by test: 8 tests of `main.rs`.** The 37 tests in `main.rs` split by hand: 8 call only `Args::parse` and `Args::synth`
(`Args` is in `wsp-base` since `c1`): `no_seat_on_reopen_takes_no_value`, `terse_is_the_flag_or_the_variable`,
`flag_person_lists_rather_than_raising`, `the_flags_that_are_flags_still_parse`,
`spawn_check_takes_the_group_and_verify_check_still_stands_alone`, `leg_add_agent_takes_its_policy_and_why_its_reason`,
`worklist_add_move_and_also_leave_the_rows_after_them_alone`, `a_dash_led_title_or_slug_behind_double_dash_is_the_positional`.
The other 29 test `help_text`, `registry::ALL`, `unknown_flags`, `misheard`, `dry_run`, `LOOKS_FIRST`, `needs_store`, which are
the binary's and stay (class (b): they check the verb table against every verb). Read by hand for `main.rs` only.

**Not found: tests in the wrong file.** A script looked for tests that name nothing their own file defines (99 of 1,897
parsed). The sample read by hand were false positives: tests that reach the file's code through a test helper
(`brought_forward` in `cmd_migrate`, `Dial` in `cmd_spawn`, `caption::say` in `stage`). No per-test list beyond `main.rs` is
claimed. The overview's prior holds: the tests are with their subject.

**Closest to (a), and why still (c).** Each is mapped to a lower crate by the layering table and reaches up, and the
upward edge is the whole job: `scratch` (store; wants `disk::{free_at,gb}`), `memory` (base, but uses `place`, so home is wire;
wants `cmd_spawn::local_backends`), `disk` (base; 7 upward lines in `components-upward.txt`: `check_tree`, `cmd_checkout`,
`cmd_sandbox`, `cycle`, `model`, `scratch`, `store`), `cmd_migrate` (store; `verification::migrate`, `barrier::migrate`).
Four files, 29 tests, 3.6 s. Their moves are code edits (seam work, `p1`/`p2` of `components.md`), not test moves.

## 7. Classification of the root test files

**(a)** the subject lives in store, wire, trees or base and the file names nothing the destination lacks: moves now.
**(b)** the tests drive a composition (the whole panel, a whole sandboxed wsp, the verb table against the help) rather than
one module: stays as an end-to-end test of the binary. **(c)** the subject is a module of engine, seats, surface, timeline or
the binary's layer, and stays until state-core cuts it. `↑` is the module's upward edges (lines in
`components-upward.txt`); `≥1 s` is how many of its tests are in the slow lane (§5). The destination for (c) is
`components.md`'s crate **as a placeholder**: `wsp-483` ("the engine and seats cut lines are replaced by this plan's") names
the real ones, and this table is the list of what has to move with them. Sorted by class, then by seconds.

| file | tests | s | ≥1 s | class | component | ↑ | subject | proposed destination / what blocks it |
|---|---|---|---|---|---|---|---|---|
| `cmd_mandate.rs` | 5 | 1.0 | 0 | a | wire | 0 | wsp mandate — standing direction for a workspace | crates/wire (wsp-wire); every `crate::` it names is already in `wsp-store`, `wsp-wire` or `wsp-base` |
| `artefact.rs` | 6 | 0.5 | 0 | a | wire | 0 | wsp artefact put / versions / restore | crates/wire (wsp-wire); every `crate::` it names is already in `wsp-store`, `wsp-wire` or `wsp-base` |
| `story.rs` | 149 | 12.3 | 1 | b | surface | 1 | wsp panel storyboard — the panel driven and drawn without a terminal | `wsp-surface`, after engine and seats are cut; drives the whole panel (panel, detail, kanban, live) from fixtures and scripted input |
| `cmd_sandbox.rs` | 16 | 6.0 | 3 | b | seats | 1 | wsp sandbox | state-core crates (components.md: `wsp-seats`); starts a whole isolated wsp with a fake herdr and the binary under test |
| `main.rs` | 37 | 4.0 | 1 | b | top | 0 | wsp — workspace and task control plane for herdr | stays; 8 of 37 → `wsp-base`; 29 test the verb table against the help; 8 are pure `Args::parse` (see §6) |
| `cycle.rs` | 125 | 205.2 | 78 | c | engine | 6 | The run's steps, taken by wsp: wsp-134 | state-core crates (components.md: `wsp-engine`) |
| `cmd_worklist.rs` | 111 | 138.6 | 69 | c | engine | 5 | wsp worklist | state-core crates (components.md: `wsp-engine`) |
| `repair.rs` | 77 | 115.3 | 41 | c | engine | 7 | What the daemon's tick repairs when no verb announced it. wsp-147 | state-core crates (components.md: `wsp-engine`) |
| `cmd_spawn.rs` | 117 | 90.6 | 25 | c | seats | 1 | wsp spawn — put a terminal, or an agent, on a piece of work | state-core crates (components.md: `wsp-seats`) |
| `worklist.rs` | 41 | 64.4 | 26 | c | engine | 0 | Where a worklist is up to, and the two readings of "finished" | state-core crates (components.md: `wsp-engine`) |
| `leg_tree.rs` | 16 | 58.0 | 13 | c | engine | 0 | A sibling's tree, and the merge back: docs/legs.md *Trees*, wsp-297 | state-core crates (components.md: `wsp-engine`) |
| `cmd_message.rs` | 38 | 46.0 | 13 | c | engine | 5 | wsp ask, wsp answer, wsp ack — the return path | state-core crates (components.md: `wsp-engine`) |
| `ending.rs` | 43 | 45.4 | 9 | c | engine | 1 | A member whose verification holds has its agent ended. wsp-193 | state-core crates (components.md: `wsp-engine`) |
| `cmd_brief.rs` | 45 | 37.3 | 12 | c | seats | 2 | wsp brief — what an agent is handed when a session starts | state-core crates (components.md: `wsp-seats`) |
| `barrier_run.rs` | 20 | 33.5 | 10 | c | engine | 0 | The barrier's mechanical part, run by wsp: wsp-326 | state-core crates (components.md: `wsp-engine`) |
| `check.rs` | 51 | 33.4 | 11 | c | engine | 1 | wsp check <task> | state-core crates (components.md: `wsp-engine`) |
| `wake.rs` | 35 | 27.9 | 10 | c | seats | 0 | Delivering an earned wake to the governor it is addressed to | state-core crates (components.md: `wsp-seats`) |
| `verification.rs` | 21 | 26.8 | 10 | c | engine | 2 | A verification is a pass on the member's own row: wsp-188 | state-core crates (components.md: `wsp-engine`) |
| `pickup.rs` | 10 | 24.9 | 9 | c | engine | 0 | The daemon starts the first planned list that may start, and says why… | state-core crates (components.md: `wsp-engine`) |
| `closing.rs` | 30 | 22.1 | 7 | c | engine | 1 | Closing a leg robustly. wsp-372 | state-core crates (components.md: `wsp-engine`) |
| `cmd_agent.rs` | 132 | 21.2 | 2 | c | seats | 5 | Agent binding, context resolution, and the views that join the store to | state-core crates (components.md: `wsp-seats`) |
| `cmd_task.rs` | 59 | 19.4 | 4 | c | engine | 5 | Task commands | state-core crates (components.md: `wsp-engine`) |
| `hold_close.rs` | 12 | 17.7 | 5 | c | engine | 0 | Closing a held question when the pane answers first, or the agent… | state-core crates (components.md: `wsp-engine`) |
| `attention.rs` | 20 | 17.5 | 4 | c | engine | 3 | The unattended pass | state-core crates (components.md: `wsp-engine`) |
| `packet.rs` | 28 | 12.7 | 4 | c | engine | 1 | wsp packet | state-core crates (components.md: `wsp-engine`) |
| `cmd_watch.rs` | 99 | 12.5 | 1 | c | seats | 2 | wsp watch | state-core crates (components.md: `wsp-seats`) |
| `hold.rs` | 14 | 11.2 | 4 | c | engine | 0 | A PermissionRequest hook that holds the call for the person's answer | state-core crates (components.md: `wsp-engine`) |
| `cmd_report.rs` | 14 | 10.8 | 3 | c | timeline | 1 | A worklist hands back outputs (wsp-375) | `wsp-timeline`, after engine and seats are cut |
| `cmd_install.rs` | 19 | 10.6 | 3 | c | seats | 1 | wsp install — put a built binary at ~/.local/bin/wsp, one at a time | state-core crates (components.md: `wsp-seats`) |
| `cold.rs` | 6 | 10.5 | 6 | c | engine | 0 | The cold build (wsp-342) | state-core crates (components.md: `wsp-engine`) |
| `cmd_resume.rs` | 31 | 10.4 | 0 | c | seats | 2 | wsp resume — offer back the agents a restart interrupted, and put the | state-core crates (components.md: `wsp-seats`) |
| `cmd_govern.rs` | 37 | 9.4 | 0 | c | seats | 1 | wsp govern — the seat that coordinates a project's agents | state-core crates (components.md: `wsp-seats`) |
| `affected.rs` | 10 | 9.4 | 4 | c | engine | 0 | Affected tests and what a test reads (wsp-361) | state-core crates (components.md: `wsp-engine`) |
| `barrier.rs` | 15 | 8.6 | 1 | c | engine | 1 | A barrier check is an entry on the list's group: wsp-230 | state-core crates (components.md: `wsp-engine`) |
| `leg_seat.rs` | 7 | 7.9 | 3 | c | engine | 2 | A leg is a seat | state-core crates (components.md: `wsp-engine`) |
| `stand_down.rs` | 9 | 7.6 | 2 | c | engine | 2 | A governor seat with nothing left to govern is stood down. wsp-337 | state-core crates (components.md: `wsp-engine`) |
| `agent_commands.rs` | 91 | 6.9 | 1 | c | seats | 0 | The second axis | state-core crates (components.md: `wsp-seats`) |
| `retro_run.rs` | 4 | 5.0 | 3 | c | timeline | 0 | The running half of the timing retro (wsp-472) | `wsp-timeline`, after engine and seats are cut |
| `verifier_packet.rs` | 7 | 4.7 | 2 | c | engine | 0 | The verification packet a React row's verifier is spawned with | state-core crates (components.md: `wsp-engine`) |
| `retro_series.rs` | 4 | 4.1 | 3 | c | timeline | 0 | The two numbered list series a timing retro files on (wsp-472) | `wsp-timeline`, after engine and seats are cut |
| `build_output.rs` | 4 | 3.7 | 1 | c | engine | 0 | A landed row's build output, dropped once nobody is working in it | state-core crates (components.md: `wsp-engine`) |
| `after.rs` | 13 | 3.3 | 0 | c | engine | 1 | Member dependencies: after:, the edge that says **this task rests on | state-core crates (components.md: `wsp-engine`) |
| `cmd_cost.rs` | 5 | 3.1 | 0 | c | timeline | 1 | wsp worklist cost <slug> [--json] | `wsp-timeline`, after engine and seats are cut |
| `cmd_project.rs` | 8 | 2.9 | 0 | c | seats | 0 | Project commands: the hierarchy and its tags | state-core crates (components.md: `wsp-seats`) |
| `panel/verbs.rs` | 28 | 2.4 | 0 | c | surface | 2 | What the letters do | `wsp-surface`, after engine and seats are cut |
| `retro.rs` | 7 | 2.1 | 0 | c | timeline | 0 | The timing retro of a cleared worklist (wsp-472) | `wsp-timeline`, after engine and seats are cut |
| `stage.rs` | 30 | 2.0 | 0 | c | engine | 2 | Stages | state-core crates (components.md: `wsp-engine`) |
| `starts.rs` | 3 | 1.9 | 0 | c | engine | 0 | A member's or a leg's start that did not take, and when it is tried… | state-core crates (components.md: `wsp-engine`) |
| `cmd_machine.rs` | 12 | 1.9 | 0 | c | seats | 1 | wsp machine — the machines agents can be run on | state-core crates (components.md: `wsp-seats`) |
| `arrange.rs` | 25 | 1.8 | 0 | c | surface | 0 | The arrange-panes port | `wsp-surface`, after engine and seats are cut |
| `legs.rs` | 32 | 1.7 | 0 | c | engine | 2 | A row run as a graph of agents: docs/legs.md, decided on wsp-287 d3 | state-core crates (components.md: `wsp-engine`) |
| `panel/rows.rs` | 22 | 1.7 | 0 | c | surface | 2 | What is in the tree, and how each row draws | `wsp-surface`, after engine and seats are cut |
| `timeline.rs` | 22 | 1.6 | 0 | c | timeline | 1 | wsp timeline <row> [--json] — where a row's wall time goes, from the | `wsp-timeline`, after engine and seats are cut |
| `panel/surface.rs` | 20 | 1.5 | 0 | c | surface | 2 | wsp surface — the panel drawn by somebody else | `wsp-surface`, after engine and seats are cut |
| `panel/run.rs` | 19 | 1.5 | 0 | c | surface | 2 | The pane: the terminal it runs in, and the loop that keeps it honest | `wsp-surface`, after engine and seats are cut |
| `cmd_attempts.rs` | 26 | 1.4 | 0 | c | timeline | 1 | wsp attempts — which tier ran a piece of work, and how it went | `wsp-timeline`, after engine and seats are cut |
| `cmd_stamp.rs` | 15 | 1.3 | 0 | c | seats | 1 | wsp stamp — has anything changed? — answered in one process start | state-core crates (components.md: `wsp-seats`) |
| `detail/editors.rs` | 16 | 1.3 | 0 | c | surface | 1 | Closing the editors a pop-out opened | `wsp-surface`, after engine and seats are cut |
| `scope.rs` | 9 | 1.2 | 0 | c | engine | 0 | What a barrier runs, scoped to what its group touched (wsp-431) | state-core crates (components.md: `wsp-engine`) |
| `draw.rs` | 15 | 1.0 | 0 | c | surface | 0 | The renderer: one spec, drawn to a terminal or to a block of text | `wsp-surface`, after engine and seats are cut |
| `room.rs` | 9 | 1.0 | 0 | c | seats | 0 | How many agents are working on this seat, against the number it will… | state-core crates (components.md: `wsp-seats`) |
| `scratch.rs` | 5 | 0.9 | 0 | c | store | 0 | A row's scratch space, with an owner: wsp-373 | `wsp-store`; `disk::{free_at,gb}` (root; `disk` reaches up, §6) |
| `panel/shared.rs` | 11 | 0.9 | 0 | c | surface | 2 | The half of a panel's view that belongs to the person rather than the… | `wsp-surface`, after engine and seats are cut |
| `did.rs` | 13 | 0.8 | 0 | c | engine | 0 | What a verifier did, as lines on its verdict: wsp-325 | state-core crates (components.md: `wsp-engine`) |
| `cmd_role.rs` | 17 | 0.8 | 0 | c | seats | 0 | Verbs whose reach depends on who is asking (wsp-473) | state-core crates (components.md: `wsp-seats`) |
| `detail/render.rs` | 16 | 0.8 | 0 | c | surface | 1 | What a task or a project looks like in full | `wsp-surface`, after engine and seats are cut |
| `panel/install.rs` | 12 | 0.8 | 0 | c | surface | 2 | Splitting the panel into a workspace, and taking it back out | `wsp-surface`, after engine and seats are cut |
| `packet_gen.rs` | 12 | 0.7 | 0 | c | engine | 0 | A packet for a row that has none (wsp-344) | state-core crates (components.md: `wsp-engine`) |
| `stuck.rs` | 9 | 0.7 | 0 | c | seats | 1 | A shell command that is waiting on itself, or on a terminal: wsp-397 | state-core crates (components.md: `wsp-seats`) |
| `memory.rs` | 11 | 0.6 | 0 | c | base | 2 | Resident memory held by idle agent panes, and a warning past a threshold | `wsp-base`/`wsp-wire`; `cmd_spawn::local_backends`; also uses `place`, so its home is `wsp-wire`, not `wsp-base` |
| `waits.rs` | 13 | 0.6 | 0 | c | engine | 1 | What a planned list waits on (wsp-383): one answer, for each planned | state-core crates (components.md: `wsp-engine`) |
| `disk.rs` | 5 | 0.5 | 0 | c | base | 7 | Free space on the volumes holding the project roots, and a warning… | `wsp-base`/`wsp-wire`; `cmd_sandbox::strays`, `scratch::find`, `cycle::log_line` (up) and `Store`/`Status` |
| `waiting.rs` | 12 | 0.5 | 0 | c | engine | 0 | Whether a seat is waiting on somebody, and on whom. wsp-172 | state-core crates (components.md: `wsp-engine`) |
| `cmd_attend.rs` | 7 | 0.4 | 0 | c | engine | 3 | wsp attend <task> — one unattended attention-harness attempt, from the | state-core crates (components.md: `wsp-engine`) |
| `daemon.rs` | 14 | 0.4 | 0 | c | top | 0 | The only long-lived piece | stays in the binary; binary's own layer; state-core replaces the daemon |
| `kanban/keys.rs` | 16 | 0.4 | 0 | c | surface | 1 | What a key does to the board | `wsp-surface`, after engine and seats are cut |
| `leaked_agents.rs` | 7 | 0.4 | 0 | c | top | 0 | Agents running for a store no daemon keeps: wsp-333 | stays in the binary; binary's own layer; state-core replaces the daemon |
| `panel/render.rs` | 5 | 0.3 | 0 | c | surface | 2 | The surface a row is drawn onto | `wsp-surface`, after engine and seats are cut |
| `kanban/mod.rs` | 7 | 0.3 | 0 | c | surface | 1 | wsp kanban — one project's work, as the four states it moves through | `wsp-surface`, after engine and seats are cut |
| `kind_data.rs` | 7 | 0.3 | 0 | c | seats | 0 | A kind that is a file rather than a Rust arm (wsp-113, reading 1) | state-core crates (components.md: `wsp-seats`) |
| `sync.rs` | 4 | 0.3 | 0 | c | seats | 0 | Projection of the durable store into herdr's sidebar tokens | state-core crates (components.md: `wsp-seats`) |
| `cmd_burn.rs` | 6 | 0.3 | 0 | c | timeline | 1 | wsp burn — where the tokens went, seat by seat | `wsp-timeline`, after engine and seats are cut |
| `kanban/render.rs` | 6 | 0.2 | 0 | c | surface | 1 | The board as styled lines | `wsp-surface`, after engine and seats are cut |
| `cmd_migrate.rs` | 8 | 0.2 | 0 | c | store | 0 | wsp migrate | `wsp-store`; `verification::migrate`, `barrier::migrate` (engine) |
| `kind_attention.rs` | 4 | 0.2 | 0 | c | engine | 1 | The attention harness as an agent kind (wsp-330) | state-core crates (components.md: `wsp-engine`) |
| `wallbuckets.rs` | 6 | 0.2 | 0 | c | timeline | 0 | Where a list's wall time went, by cause (wsp-470) | `wsp-timeline`, after engine and seats are cut |
| `cmd_calls.rs` | 3 | 0.2 | 0 | c | timeline | 0 | wsp calls — what wsp's own calls cost (wsp-481) | `wsp-timeline`, after engine and seats are cut |
| `cmd_verifications.rs` | 3 | 0.1 | 0 | c | timeline | 1 | wsp verifications — what verifiers did, as a running number: wsp-325 | `wsp-timeline`, after engine and seats are cut |
| `history.rs` | 5 | 0.1 | 0 | c | engine | 0 | A list's history as data: wsp-254 (compound docs/ux-design.md §19.3 | state-core crates (components.md: `wsp-engine`) |
| `readonly.rs` | 4 | 0.1 | 0 | c | top | 0 | Which verbs write nothing (wsp-488): the one place that says so | stays in the binary; binary's own layer; state-core replaces the daemon |
| `split.rs` | 4 | 0.1 | 0 | c | engine | 0 | A list member that turns out bigger than one seat, and the two ways… | state-core crates (components.md: `wsp-engine`) |
| `live.rs` | 3 | 0.1 | 0 | c | surface | 0 | What is running, in wsp's own words | `wsp-surface`, after engine and seats are cut |
| `barrier_replay.rs` | 2 | 0.1 | 0 | c | engine | 1 | wsp worklist replay | state-core crates (components.md: `wsp-engine`) |
| `detail/run.rs` | 1 | 0.0 | 0 | c | surface | 1 | The pane itself | `wsp-surface`, after engine and seats are cut |

Totals (the row's answer):

| class | files | tests | seconds | tests ≥ 1 s |
|---|---|---|---|---|
| (a) could move now | 2 files + 8 tests of `main.rs` | 19 | 2.1 | 0 |
| (b) end-to-end, stays | 3 files (`main.rs` at 29 tests) | 194 | 21.6 | 5 |
| (c) stays until state-core | 89 | 1,996 | 1,241.3 | 399 |
| **root crate** | **94** | **2,209** | **1,265.0** | **404** |

Within (c), by the component that will cut it: engine 38 files, 933 tests, 958 s; seats 17 files, 687 tests, 236 s;
surface 16 files, 222 tests, 15 s; timeline 11 files, 100 tests, 29 s; the binary's layer (`daemon`, `leaked_agents`, `readonly`)
3 files, 25 tests, 1 s; and the four near-(a) files above (store 2 files and base 2 files, 29 tests).

`tests/layering.rs` (2 tests, 0.9 s) is not in the table: it is already an integration test and reads the whole tree, so it
is class (b). The brief said 93 files; the count at this commit is 94 by `grep -c '#[test]'`; the extra file was not traced.

## 8. For the state-core plan (`wsp-483`)

What this audit hands the plan, for its components section and the testing paragraph of Ed's note:

1. **1,996 of the root's tests (90%) can move only when their module is cut.** The table in §7 is the move list, file by
   file, with the upward edges that block each. Engine is 933 of them and 958 s; seats 687 and 236 s.
2. **Where tests live per new crate** is mostly settled by that table: a root test sits in the `#[cfg(test)]` module of the
   file it tests and moves with it. The exceptions are the 29 `main.rs` tests (the verb table stays in the binary) and class
   (b), which want a place of their own: a binary-level `tests/` target or an e2e dev-crate.
3. **Tier by time, not file** (§5): the 528 tests ≥ 1 s are 73% of the time and the engine's files hold 336 of the root's 404.
   A pure transition test for a state machine replaces a test that today builds a repository and steps `cycle`; the
   336 slow engine tests are the list of what the machine tests would replace.
4. **The barrier's scoping** cannot narrow while the root is one crate: 73% of the seconds are in it. After the cut it
   narrows to seats (14%) or surface/timeline/top (under 4%), and not for the engine's 56% except through the slow lane.
5. Re-run the measurement after each cut: `cargo nextest run --workspace` with a JUnit config (§2) is the whole procedure,
   and the per-file table regenerates from the test names.
