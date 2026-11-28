# The exit packet: one structured, validated way out of a seat

Reference, as built. Planned in wsp-610 (list `exit-packet`, 2026-11-12) and
built as rows E1–E7 in groups 2–8 of that list (wsp-612 … wsp-618). Ed asked for
it on 2026-11-12: "we inject a packet to the agent for its instructions, how can
we make the end more reliable?"

A seat comes in through one generated, checked document: the packet
(src/packet.rs). It now leaves the same way. It writes one file in a fixed
shape to `$WSP_EXIT`, runs one verb, `wsp submit`, and wsp validates the whole
file before it moves anything. An accepted exit ends the seat. A turn that stops
without one is told at once what is missing.

Every section, rule and alias below names the test that holds it, as
`module::test_name`. Where a rule has no test of its own yet, the line says
**untested** and names what is owed (§12).

| Where | What |
|---|---|
| src/exit.rs | The contract: one `Exit` type for the four kinds, its text parse and render, its JSON form, typed asks, the outcome. No IO. (wsp-612, E1) |
| src/check.rs | The one check-line grammar (`timed_line`), a project's `## Artefacts` and `## Full suite` (D4). (wsp-609, wsp-612, wsp-613) |
| src/cmd_submit.rs | `wsp submit`: the validator V0–V7 and what an accepted exit does, for every kind. (wsp-613, wsp-614) |
| src/ending.rs | `end_exit`: the accepted exit's seat ended at its `Stop`. (wsp-615, E4) |
| src/repair.rs, src/closing.rs, src/waiting.rs | The reminder at the stop, the prompt raised at once, closing for the seat. (wsp-616, E5) |
| src/closing.rs, src/packet.rs, src/verifier_packet.rs, src/cycle.rs, src/leg_seat.rs | The template every packet ends with, and the one line of how to close. (wsp-617, E6) |

## 1. Why: where seats lost time at the exit

Window: ~/.local/state/wsp/cycle.log from 2026-11-11T17:00Z to 2026-11-12T10:10Z
(2,152 lines), the rows named below, and the primary seat's overnight.log.

| Row | What the exit did | Cost | What stops it now |
|---|---|---|---|
| compounding-447 | Auto mode denied its `git checkout` of two check-written shots. It ended on a prose question and filed no ask | 4 min 20 s until Ed typed | V1 (wsp restores the project's artefacts itself), S1 |
| compounding-439 | Sent its "ready" report as a question | Barrier 9 held 8½ h (wsp-608) | V5 (an ask that asks nothing is a note) |
| compounding-442 | Wrote a check as `web-chrome ctl and workspace: cmd`, with no `(time)` | The barrier ran ~/.local/bin/web-chrome, 2 × 600 s (wsp-609) | V3 (one strict grammar) |
| 459 and others | Recorded the full `npm run check` (~15 min) as their own check | Rerun 3–4 times per row | V3 (scope), D4 |
| 366, 409, 447, 450, 454 | Reviewed with check-written files in the tree | Lands refused ~2 h each | V1 |
| 431 | Reviewed with uncommitted work | Sent back before verification | V1, refused at submit |
| 449 (verifier) | Lost its tree and stopped at an AskUserQuestion menu | Barrier waited ~20 min | `cause: env`, S2 |
| ~15 seats | Finished, but the seat stayed up | 13 idle panes holding 2.8 GB while the machine thrashed | §5 (D1) |
| verifier blocks | 66 of 73 blocks recorded no cause | A send-back the next seat had to rediscover | `cause:` |

Not the exit's: 30 of 43 send-backs in the window were "the rebase onto master
does not apply cleanly", 7 of them "there is already a rebase-merge directory".
That is the land's own leftover state, named to the governor and not built here.

## 2. The file

The seat writes it to `$WSP_EXIT`. A spawn sets `$WSP_EXIT` beside
`$WSP_VERDICT` (the same path, `closing::verdict_path`) and `$WSP_SEAT_KIND`;
a leg's seat gets `$WSP_EXIT` and `$WSP_SEAT_KIND=leg` too. Sections are fixed,
come in this order, and each starts at the beginning of a line. Every section
that does not apply to the kind is refused.

- `cmd_spawn::a_spawned_members_seat_has_its_exit_path_beside_its_verdict_and_kind`
- `exit::each_kind_round_trips_through_text_and_json`
- `exit::every_refusal_is_returned_at_once_and_each_kind_keeps_its_own_sections`

### 2.1 A member

```text
summary: <under 100 characters: what is true now>

<the account: what you did, what you left, what the next reader needs>

done-when:
- held: <the item, as the packet says it> — <where: a test name, file:line>
- not held: <the item> — <why>

checks:
- <label> (<time>): <command>

asks:
- governor: <a decision the list's governor can make>
- person: <a decision only the person can make>
- note: <something worth reading that asks nothing>

owed: nothing
```

`owed:` is `owed: nothing`, or `owed:` with one `- <what is left>` per line under
it. A line `- person: <step>` is a step only the person can do
(compounding-470): the row still goes on, the step is logged by name, and the
governor is told it belongs to the list's closing person row
(`cmd_submit::steps_owed_to_the_person_leave_the_row_going_on_and_are_logged_by_name`).

### 2.2 A verifier

```text
verdict: holds | blocks
summary: <one line>

<the verdict in prose>

done-when:
- held: <item> — <evidence you read or ran>
- not held: <item> — <what is wrong, file:line>

checked:
- <label> (<time>): <command> — pass | fail [— <note>]

cause: work | check | tree | env — <one sentence>     (required on blocks)

asks:
owed:
- <what the member owes; becomes the `sent back:` line>   (required on blocks)
```

A verifier's checks are `checked:`, never `checks:`, and each says what it came
to (`— pass` or `— fail`). `cause:` routes a block:

| Cause | What happens | Test |
|---|---|---|
| `work` | The row is sent back with `owed:` as the `sent back:` reason; packet::owed prints it to the next seat | `verification::a_verifier_exit_that_blocks_on_the_work_sends_the_row_back_with_its_owed_lines_as_the_reason` |
| `check` | Sent back the same way; `checked:` names the failing check | the same test's path; **untested** as its own case |
| `tree` | Sent back, and the member's log says it is a defect in V1: `submit: a \`tree\` block (…) — commits missing or the tree dirty, which V1 should have refused at the member's submit: a defect in the gate` | **untested** |
| `env` | **No block.** The pass is recorded `could not verify`, the member stays at review, and the run starts a new verifier | `verification::a_verifier_exit_with_cause_env_leaves_the_row_at_review_and_the_run_owes_a_new_verifier` |

A `blocks` without a `cause:` is refused naming the line
(`verification::a_verifier_exit_that_blocks_without_a_cause_is_refused_naming_the_line`).
A `holds` with an item not held or a check failed is refused
(`verification::a_verifier_exit_that_holds_with_a_failed_check_is_refused`).

### 2.3 A barrier check

The verifier's shape, with `verdict: go | hold | followup`. The `<row>` it
submits for is its list (or `<list>-check-<n>`). `go` and `hold` run as those
verbs, a `hold` must carry a `cause:`, and `followup` stays a flag to the
person, because the rows it names are not ones wsp can guess.
`closing::a_checks_exit_holds_only_with_a_cause_and_a_followup_stays_a_flag_to_the_person`.

### 2.4 A leg

`summary:`, the account, `checks:` and `owed:`. `done-when:` is optional: the
done-when is the leg order's. Its submit is `wsp leg done` with the file as the
baton, and the last leg's submit closes its leg and ends the row as a member's
would.

- `closing::a_leg_seats_submit_closes_the_leg_as_leg_done_does_with_the_file_as_its_baton`
- `closing::a_leg_exit_that_does_not_validate_leaves_the_leg_open_and_leg_done_runs_the_same_rules`

### 2.5 The JSON form

`Exit::json` / `Exit::from_json` carry the same type, as wsp-483's state core
`submit` message (docs/state-core.md §5.1) will. Reading JSON renders it and
parses the text again, so a payload obeys the file's grammar exactly; a payload
that does not read back as itself is refused (`exit`: "the payload does not read
back as itself as a file"). `exit::each_kind_round_trips_through_text_and_json`.

## 3. The verb

```text
wsp submit <id> [--from FILE|-] [--dry-run]
```

- With no `--from` it reads `$WSP_EXIT`, else the place wsp keeps for it
  (`closing::verdict_path`).
- The kind is `$WSP_SEAT_KIND`'s, never the file's, so a member cannot submit
  a verdict. Unset is a member. A word that is no kind is refused: `wsp:
  $WSP_SEAT_KIND is \`<word>\`, which is no seat kind (member, verifier, check,
  leg)` (`cmd_submit::a_word_that_is_no_seat_kind_is_refused`).
- It runs every rule of §4 and prints **every** refusal at once, then exits 1
  and moves nothing: `wsp: <id> not submitted — N refusal(s) in <file>; nothing
  moved. Fix the file and \`wsp submit <id>\` again (\`--dry-run\` checks it
  first)`. A leg's says `the leg is still open`; a verifier's says `nothing
  recorded`.
- `--dry-run` validates and prints what it would do, and moves nothing
  (`cmd_submit::an_accepted_exit_moves_the_row_to_review_with_the_file_as_its_account`).
- Accepted, it does what the old verb did, records the file whole as the row's
  account, files each ask through `waiting::route`, and ends the seat (§5).

Each refusal is one line:

```text
line <n> (<section>): <what was wrong> — write <the text to write instead>
```

`cmd_submit::three_faults_in_one_file_are_three_refusal_lines_in_one_run`.

### 3.1 The aliases

| Verb | As built | Test |
|---|---|---|
| `wsp review <id> "…"` | From a run's seat its words are read as an exit (summary = first line, account = the rest, `owed: nothing`) and run through the validator. What it would refuse is printed as `wsp: warning (a run seat's review is read as an exit; \`wsp submit\` will refuse this) — <line>`, and the review goes on (D2). Outside a run, unchanged | `cmd_submit::a_bare_review_from_a_run_seat_warns_with_the_validators_refusals_and_outside_a_run_says_nothing` |
| `wsp verified <id> --holds/--blocks` | The verifier kind's submit. Given exit-shaped text it is validated as the file; given prose, the flag is the `verdict:` and `--cause "work — …"` the cause. A `--blocks` with no cause is refused | `verification::a_verifier_exit_that_blocks_without_a_cause_is_refused_naming_the_line` |
| `wsp worklist go/hold` from a check seat | The check kind's submit, through `cmd_submit::check_seat_words`. The person's and the primary's `go`/`hold` are unchanged: they are decisions, not exits | `closing::a_checks_exit_holds_only_with_a_cause_and_a_followup_stays_a_flag_to_the_person` |
| `wsp leg done` | The leg kind's submit: the baton goes through `cmd_submit::leg_baton`, so it runs the same rules on an exit-shaped baton | `closing::a_leg_exit_that_does_not_validate_leaves_the_leg_open_and_leg_done_runs_the_same_rules` |
| `wsp check --set` | Stays, for declaring checks mid-work, through the same grammar. At the exit, `checks:` writes the row's `## Checks` | `check::a_timed_label_is_everything_before_the_last_time_and_the_exits_grammar_is_the_same_function` |
| `wsp flag`, `wsp ask` | Stay, mid-work: a hand raised while the seat keeps working, or a question it waits on. The exit's `asks:` covers the end | — |
| `wsp block`, `wsp park` | Stay, for a person or a governor. A run seat's stop on a decision is an exit with a `not held:` line and a decision ask (§4.1) | `cmd_submit::a_not_held_with_a_decision_ask_blocks_the_row_on_it` |

## 4. The validator

The rules run in order, and all of them run, whatever an earlier one found.

**V0. Shape** (`exit::Exit::parse`). Every section is known, in order, and once.
`summary:` is present; `done-when:` for a member; `verdict:` for a verifier or a
check. The refusals:

- `"<Name>:" is not a section of the exit — write it inside a section — the sections are …`
- `"<line>" is outside a section — write it in the account, under \`summary:\` …`
- `the exit has no \`summary:\``, `a member's exit has no \`done-when:\``,
  `a verifier's exit has no \`verdict:\``
- `a verifier's checks are \`checked:\`, not \`checks:\``, `` `<name>:` comes twice ``
- `"<word>" is not a verdict`, `"<word>" is not a cause`,
  `the cause carries no sentence after \` — \``

Tests: `exit::an_unknown_section_a_missing_summary_and_an_untyped_ask_fail_with_their_line`,
`exit::every_refusal_is_returned_at_once_and_each_kind_keeps_its_own_sections`.

**V1. The tree.** `git status --porcelain --untracked-files=all` is empty, or
every path in it matches the project's `## Artefacts` (§4.2). wsp restores those
paths itself, in its own process (tracked: checked out; untracked: removed by
exact path), so it works where a seat's own `git checkout` was denied by auto
mode, and logs `submit: restored the project's ## Artefacts: <paths>` on the
row. Anything else dirty is refused by name, and nothing is restored:

```text
wsp: <tree> has uncommitted work that is not one of the project's ## Artefacts, so <id> is not submitted
  changed, tracked: …
  new, untracked — a check's output, or work nobody committed: …
  commit what is work (`wsp commit-help`) and revert what a check wrote (`git -C <tree> checkout -- <file>`); …
```

treecopy's mismatch (`review_refusal`) runs here too.
Tests: `cmd_submit::a_tree_dirty_only_with_the_projects_artefacts_is_restored_and_accepted_with_the_paths_logged`,
`cmd_submit::another_dirty_path_is_refused_by_name_and_nothing_is_restored`.

**V2. The commits.** A commit a verifier already blocked, put up again
unchanged, is refused with wsp-529's words (write `a new commit with the fix, or
--anyway "why"`): `verification::a_real_block_at_an_unchanged_commit_is_still_refused`.
Zero commits ahead of the trunk is accepted and logged `submit: nothing to
land`: **untested** as its own case.

**V3. Checks.** Each line parses strictly as `label (time): command` through
`check::timed_line`, the one grammar `## Checks`, `wsp check --set`, `wsp
worklist stop-checks` and the barrier runner share (wsp-609). The label is
everything before the last ` (` that a time and `): ` follow, so it may be
several words, and only the command is ever run. A line it cannot read is
refused with the shape to write and never becomes a command. Then, for a
member:

- a project barrier or end step is refused: `"<cmd>" is one of the project's
  \`at barrier\` steps, which the barrier runs`;
- a full-suite command (`cargo nextest run --workspace`, `npm run check`, or one
  the project's `## Full suite` names) is refused, `"<cmd>" is the barrier's
  full suite`, unless that section says `members: allowed` (D4); then it is let
  through and logged `full-suite: …`;
- a time past 30m: `` `<label>` is given <time>, past a member's 30m ``;
- a command `sh -n` rejects, or whose first word is no builtin, program on PATH
  or path in the tree.

A `full-suite:` line in the exit itself is refused: "the full-suite override is
not an exit's line", because the override is the project's (D4).

Tests: `exit::a_label_of_several_words_parses_and_only_its_command_is_the_command`,
`exit::a_check_line_without_a_time_is_refused_never_a_command`,
`check::a_timed_label_is_everything_before_the_last_time_and_the_exits_grammar_is_the_same_function`,
`cmd_submit::a_barrier_step_is_refused_as_a_members_check` (barrier step, full
suite, and `members: allowed`). The 30m limit and the `sh -n` / first-word
refusals are **untested** at submit (the first word is tested at `wsp check
--set` by wsp-609).

**V4. Done-when.** Each line opens with `held:` or `not held:` and has an item
and a reason either side of ` — `. When the row has a `### Done when` (or `Done
when:`) paragraph of bullets, the count must match (`the row's Done when has N
item(s) and this has M`) and each line must open with its item's first three
words (`"…" is not the row's item N`). Prose done-whens are not counted.
`cmd_submit::a_not_held_with_nothing_owed_is_refused_and_the_rows_done_when_bullets_are_counted`.

**V5. Asks.** Each line is typed `governor:`, `person:` or `note:`; an untyped
one is refused (`"…" is not typed`). A `governor:` or `person:` ask that
`waiting::Words::asks` reads as asking nothing is **filed as a note and said
so**, not refused: `line N (asks): the \`person:\` ask asks nothing (no \`?\`, no
"Need …", no "Either … or"), so it is filed as a note, not a question`. Every
question goes through `waiting::route`.
`cmd_submit::a_person_ask_that_asks_nothing_is_filed_as_a_note`.

**V6. The account.** `summary:` is non-empty (`the summary is empty`) and under
100 characters (`the summary is N characters`); the prose under it is non-empty
(`the exit has no account`), which is review's robustness-101 gate, kept. The
empty summary is held by
`exit::an_unknown_section_a_missing_summary_and_an_untyped_ask_fail_with_their_line`;
the 100-character and no-account refusals are **untested** at submit.

**V7. Owed.** `owed:` is present (`the exit has no \`owed:\``). Every `not held:`
has an `owed:` line or a decision ask it waits on (`"…" is not held, and nothing
is owed for it or asked`). A verifier's `blocks` needs an `owed:` line, because
it becomes the `sent back:` the next seat reads.
`cmd_submit::a_not_held_with_nothing_owed_is_refused_and_the_rows_done_when_bullets_are_counted`.

### 4.1 The outcome is read from the file, not chosen

`exit::Exit::outcome`, held by `exit::the_outcome_is_read_from_the_file`.

| The file says | A member's row | The seat | Test |
|---|---|---|---|
| every item `held` | `review`, ready at the tree's commit; the verifier runs | ended (§5) | `cmd_submit::an_accepted_exit_moves_the_row_to_review_with_the_file_as_its_account` |
| any `not held` and a `governor:`/`person:` ask | `blocked` on that ask, routed | ended; the answer reopens the row with it as the `sent back:` reason (D3) | `cmd_submit::a_not_held_with_a_decision_ask_blocks_the_row_on_it`, `cmd_submit::the_answer_to_a_not_held_exits_ask_reopens_the_row_with_it_as_the_reason` |
| any `not held` and only `owed:` | `review` as partial; the verifier reads `owed:` | ended | `exit::the_outcome_is_read_from_the_file` |
| any `not held`, nothing owed or asked | refused (V7) | stays | `cmd_submit::a_not_held_with_nothing_owed_is_refused_and_the_rows_done_when_bullets_are_counted` |

Acceptance runs **the review core** (`cmd_task::accept`, every gate `wsp review`
has) with the file whole as the account, after `checks:` has written the row's
`## Checks`.

### 4.2 The project's keys

Both live in the project file, `~/wsp/projects/<project>.md`, beside `##
Checks`, and both are the person's to write.

```markdown
## Artefacts
# one glob per line: paths a check always rewrites, which wsp restores at submit

## Full suite
- `cargo nextest run --workspace`
members: allowed
```

`## Artefacts` is one glob per line (a `- ` bullet and backticks allowed),
matched as `check::matches` matches a rule's `on`. A line starting `#` is a
comment (an HTML comment is not: it would be read as a glob). An empty section,
or one holding only comments, reads as none. `## Full suite` names the project's
full-suite commands; the line `members: allowed` lets a member's check or a run
step below the barrier use them, logged. Without it a member's check naming one
is refused (V3). `check::a_projects_artefact_globs_are_read_and_an_empty_section_reads_as_none`
holds both parsers.

## 5. An accepted exit ends the seat

At acceptance the seat holds no place under the cap from that call on, and
`ending::end_exit` ends it at its `Stop`, by its pid tree, with the tree kept: a
member's (D1), a verifier's and a check's. ending.rs's guards still apply: a
governor seat is never ended, and neither is a seat a person has typed to since;
each is said once. A seat still mid-turn is left for its stop and counts for
nothing meanwhile. A seat already gone or ended is said and forgotten; one
whose pid outlives the despawn is said "not ended" once and not retried. Every
older ending steps aside for an exit seat, so there is one ending per seat.

- `ending::a_member_a_verifier_and_a_check_are_ended_within_one_tick_of_an_accepted_exit_and_the_tree_kept`
- `ending::a_governor_seat_and_one_typed_to_since_are_not_ended_and_each_is_said_once`
- `ending::a_seat_mid_turn_is_left_for_its_stop_and_counts_for_nothing_meanwhile`
- `ending::a_verifier_and_a_check_mid_turn_are_left_for_their_stop_by_every_ending`
- `ending::a_seat_already_gone_or_already_ended_is_said_and_forgotten`
- `ending::a_seat_whose_pid_outlives_the_despawn_is_said_not_ended_once_and_not_retried`

## 6. Every packet ends with its template

`closing::exit_template(kind, row, done)` is the template for the seat's kind,
under `## Your exit`, in section order, then one line of how to close. It is
under `TEMPLATE_MAX` (800 bytes) before the items. A member's `done-when:` comes
pre-filled from the row's own bullets as `- held: <item> — ` for the seat to
finish or flip. The member's is at the end of `packet::render`, a leg's in
`build_leg`, and the verifier's and the check's at the end of theirs.

The line of how to close (`closing::submit_words`) is the same everywhere:

> Close in three Bash calls, none chained: write it with a heredoc, `cat >
> "$WSP_EXIT" <<'EOF'` ... `EOF` (never a bare `cat`); `wsp submit <row>
> --dry-run` once, fixing what it refuses; then `wsp submit <row>` alone.

- `closing::each_seats_exit_template_has_its_sections_in_order_and_fits`
- `closing::a_templates_done_when_is_pre_filled_from_the_rows_items`
- `packet::a_packet_carries_the_member_template_with_the_rows_done_when_pre_filled`

## 7. A turn that stops without a submit

**S1. At the seat's `Stop`.** A run seat whose turn stops with no exit accepted
since it started, and that did not end wanting somebody, is told at once, once
per stop: `no exit at $WSP_EXIT`, or the dry run's refusal lines for the file as
it stands, then the one command. A turn that does want somebody (a request in
its last words, a denied call) is raised as wsp-606 built it. The
end-and-restart window after the first tell stays.

- `repair::an_idle_member_with_no_exit_is_told_at_its_stop_once_per_stop_and_still_ended_a_window_after_the_first_tell`
- `repair::a_member_with_a_refused_exit_is_told_its_refusal_lines_at_its_stop`
- `closing::a_verifier_whose_turn_stops_with_no_exit_is_told_at_once_once_per_stop_and_flagged_a_window_after_the_first_tell`

**S2. A seat at an on-screen prompt** (an AskUserQuestion menu or a permission
prompt) in a run is raised at once through `waiting::route`, with the prompt's
text. `closing::a_verifier_at_an_ask_user_question_prompt_is_raised_through_its_route_within_one_tick`
holds the verifier's; a member's and a check's go through the same route, and
are **untested** as their own cases.

**S3. Closing for the seat stays as the backstop.** A seat idle a window after
the reminder, with a valid exit written, is submitted by wsp. A file that does
not validate is a flag to the person with its refusal lines. wsp's close of a
leg runs the leg validator, and a valid one closes with its summary as caption.

- `closing::a_member_seat_idle_past_the_window_with_a_valid_exit_is_submitted_and_a_refused_one_is_flagged_with_its_lines`
- `closing::a_verifier_seat_idle_past_the_window_with_a_valid_exit_is_submitted_by_wsp`
- `closing::wsps_close_of_a_leg_runs_the_leg_exit_validator_a_refused_exit_is_flagged_with_its_lines_and_stays_open`
- `closing::wsps_close_of_a_leg_with_a_valid_exit_closes_it_with_the_exits_summary_as_caption`

**A silent check seat is seated again, once** (wsp-903). A barrier check seat of
a running list that is idle 10 minutes after the reminder, with no verdict file
and no shell of its own, is ended by wsp when the reminder was delivered and the
seat reads as having ended no turn and begun none since. Its entry is marked
`silent`, and the run seats another. This happens once per verdict on the
barrier. The next silent one is flagged and left. A seat that answered the
reminder, that the reminder did not reach, that has a shell of its own, or that
nothing can say began a turn is flagged as before.

- `closing::a_silent_check_seat_is_ended_and_seated_again_once_and_the_second_is_flagged`
- `closing::a_check_seat_that_answered_the_tell_is_flagged_and_not_ended`
- `closing::a_check_seat_nothing_can_say_a_turn_of_is_flagged_and_not_ended`
- `closing::a_check_seat_the_tell_did_not_reach_is_not_ended`
- `closing::a_check_seat_with_a_shell_of_its_own_is_flagged_and_not_ended`

## 8. What it absorbed

- **wsp-606**: its raise is S1's "wants somebody" branch, unchanged.
- **wsp-608**: V5 routes every ask through `waiting::route`.
- **wsp-609**: V3's grammar is wsp-609's `timed_line`, built once.
- **wsp-605**: `dirty_files` / the dirty refusal are V1's words; its land-owed
  retry stays as the backstop.
- **wsp-385**: §5 extends the ending to members, at the verb.
- **compounding-456**: `npm run check` now writes its shots to a temp dir unless
  `BLESS=1`, so compounding has no artefact to list (D5).

Relative to wsp-483 (the state core): the exit is the payload of its `submit`
message, and the validator is the guard on the row machine's `doing → review`
and the pass machine's `running → holds|blocks`. The JSON form (§2.5) is what the
server will store.

## 9. The build

| Group | Row | What |
|---|---|---|
| 2 | wsp-612 (E1) | src/exit.rs, the shared check-line grammar |
| 3 | wsp-613 (E2) | `wsp submit` for members, V0–V7, `## Artefacts`, `## Full suite`, `review` as the alias |
| 4 | wsp-614 (E3) | `submit` for verifiers, checks and legs; `verified`, a check's `go`/`hold`, `leg done` as aliases |
| 5 | wsp-615 (E4) | an accepted exit ends the seat |
| 6 | wsp-616 (E5) | S1–S3 |
| 7 | wsp-617 (E6) | the template in every packet |
| 8 | wsp-618 (E7) | this reference, the help, the brief wording raised to Ed |

## 10. Decisions, as decided (Ed, 2026-11-12)

1. **D1: end the member's seat at submit.** Decided: yes, at submit, not at the
   verdict. Built (§5).
2. **D2: how long does a bare `review` from a run seat stay a warning?**
   Decided by the primary seat on the plan's recommendation: a warning until one
   full list has run on `submit` with no stall at the exit, then refused. Built:
   the warning (§3.1). The switch to a refusal is not yet made; it is owed once a
   list has run clean.
3. **D3: a `not held` with a decision ask ends the seat**, and the answer
   reopens the row with the answer as its `sent back:` reason. Built (§4.1).
   Ed also asked that **the time from the ask to the decision be recorded**: the
   answer logs `decided: <question> answered Nm after it was asked` on the row
   and a `decision-waited` event with the seconds. Not yet read into the effort
   and retro data. And **a waiting row that holds up a lot of work segments the
   worklist**: the blocked row, and only what rests on it, moves to a later
   group so the rest goes on. Not yet built. Both are owed (§12).
4. **D4: the full-suite override is the project's**, in `~/wsp/projects/<p>.md`
   beside `## Checks`, not a member's `checks:` line or only an env var. Built as
   `## Full suite` with `members: allowed` (§4.2); an exit's own `full-suite:`
   line is refused. `WSP_FULL_SUITE` stays for a seat's own run of the suite.
5. **D5: compounding's `## Artefacts`** starts empty, or with a comment that it
   is there for future artefacts. The plan's `docs/web-chrome/plugins/shots/*.png`
   line is obsolete since compounding-456. The project file is Ed's: the line is
   proposed to him on wsp-618 (§11), not written.

## 11. The brief, the handbook and the artefact line (Ed's to edit)

These are raised to Ed as one flagged ask on wsp-618. wsp does not edit
~/wsp/agents.md, the project handbook or the project files.

**~/wsp/agents.md.** Replace the line "Finished? `wsp review`. …" with:

> - Finished, or stopping for any reason? Your packet ends with the exit. Write
>   it to `$WSP_EXIT` with a heredoc in one Bash call, run `wsp submit <id>
>   --dry-run` once and fix what it refuses, then `wsp submit <id>` as a call of
>   its own. Every refusal names its line and what to write. An accepted exit
>   ends your seat. `done` belongs to the person you are working for.
> - Never end a turn on a question in prose. Nobody reads your screen. A
>   question at the end goes in the exit's `asks:` (`governor:` or `person:`). A
>   question mid-work is `wsp ask`.

In "Need a decision?", replace "`wsp block <id> \"the question\"`, say so, and
stop" with "put it in the exit as a `not held:` line and a `governor:` or
`person:` ask, and submit; the answer reopens the row", and keep the rest.

**The wsp handbook, "What an agent is handed".** Add: "Every packet ends with
the exit template for the seat's kind (member, verifier, check, leg), its
done-when pre-filled from the row, and one line of how to close: a heredoc into
`$WSP_EXIT`, `wsp submit <id> --dry-run` once, then `wsp submit <id>`. The seat
carries `$WSP_EXIT` and `$WSP_SEAT_KIND`." (`wsp project edit wsp --handbook -`.)

**~/wsp/projects/compounding.md.** Its first `## Artefacts`:

```markdown
## Artefacts
# none yet: paths a check always rewrites go here, one glob per line, and wsp restores them at submit (compounding-456 sends the shots to a temp dir unless BLESS=1)
```

## 12. Owed

- D2's switch from warning to refusal, once one list has run on `submit` with no
  stall at the exit.
- D3's decision time read into the effort and retro data, and the worklist
  segmented around a decision that holds up much of a group.
- Tests of their own for: `cause: check` and `cause: tree`, V2's `nothing to
  land`, V3's 30m limit and `sh -n` / first-word refusals at submit, V6's
  100-character and no-account refusals, and S2 for a member's and a check's
  prompt.
