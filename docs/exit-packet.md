# The exit packet: one structured, validated way out of a seat

Plan, wsp-610 (list `exit-packet`), 2026-11-12. It builds nothing. It reads the
evidence, sets the contract, and files the build as rows in groups 2–8 of
`exit-packet`. Ed asked for it on 2026-11-12: "we inject a packet to the agent for
its instructions, how can we make the end more reliable?" He agreed the shape and
said to run the list after react parity.

A seat comes in through one generated, checked document: the packet
(src/packet.rs). It leaves through whatever it improvises from five verbs and its
own last message. This plan makes the way out as fixed as the way in. There is
one file in a fixed shape, one verb, and wsp validates the file before it accepts
anything. An accepted exit ends the seat. A turn that ends without one is told at
once what is missing.

## 1. The evidence: where seats lost time at the exit

Window: ~/.local/state/wsp/cycle.log from 2026-11-11T17:00Z to 2026-11-12T10:10Z
(2,152 lines). Also the rows named below and the primary seat's
~/.claude/jobs/bc46b9cd/tmp/overnight.log.

| Row | What the exit did | Cost | Which rule below stops it |
|---|---|---|---|
| compounding-447 | Auto mode denied its `git checkout` of two check-written shots. It ended on a prose question and filed no ask | 4 min 20 s until Ed typed | V1 (wsp reverts known artefacts itself), S1 |
| compounding-439 | Sent its "ready" report as a question | Barrier 9 held 8½ h (wsp-608) | V5 (`note:` is never a question) |
| compounding-442 | Wrote a check as `web-chrome ctl and workspace: cmd`, with no `(time)` | The barrier ran ~/.local/bin/web-chrome, 2 × 600 s (wsp-609) | V3 (strict grammar) |
| 459 and others | Recorded the full `npm run check` (~15 min) as their own check | Rerun 3–4 times per row | V3 (scope) |
| 366, 409, 447, 450, 454 | Reviewed with check-written files in the tree | cycle.log: 3 "land is owed — the tree is dirty" tells and 3 "ending … with 2 uncommitted path(s)" lines in the window. Lands refused ~2 h each | V1 |
| 431 | Reviewed with uncommitted work | Sent back at 00:38:57Z, before verification | V1 (refused at submit, where its author is) |
| 449 (verifier) | Lost its tree and stopped at an AskUserQuestion menu | Barrier waited ~20 min | Verifier `cause: env`, S2 |
| ~15 seats | Finished, but the seat stayed up | Places held until ended by hand. overnight.log 14:23: "13 idle panes holding 2.8 GB" while the machine thrashed | A1 |
| verifier blocks | 66 of 73 blocks in the store record no cause (compound-1017's mock-up) | A send-back the next seat has to rediscover | Verifier `cause:` |

Not the exit's fault, but it shows up in the same window: 30 of 43 send-backs
were "the rebase onto master does not apply cleanly", and 7 of those were "there
is already a rebase-merge directory". That is the land's own leftover state. It
is named to the governor (§9) rather than folded in here.

### What already exists, and which parts this plan keeps

The exit is not a blank slate. Most of the gates exist. They are spread across
verbs and run at different moments:

- `wsp review` (src/cmd_task.rs:1652) refuses an empty account (robustness-101).
  It refuses a tree that is not the one `verify` ran on (`treecopy::review_refusal`,
  :1725), a dirty tree (`cmd_checkout::ready_at`, :1759), a commit a verifier
  already blocked (:1775, wsp-529), and `## Checks` lines that `sh -n` rejects
  (:1801).
- Verdict files at a known place (src/closing.rs:176 `verdict_path`, `$WSP_VERDICT`).
  `parse_verdict` (:201) accepts a first word (`holds`/`blocks`, `go`/`hold`/`followup`)
  and free prose. `how_to_close_verdict` (:237) is the closing order the seats
  read.
- Closing for the seat: `close_member` (:659), `settle_verifier` (:737) and
  `settle_check` (:790). They close from a written verdict after a window.
- The turn's end is already read: `waiting::TurnEnd` (src/waiting.rs:239: last
  words, `asks`, `denied`), and wsp-606's `raise_turn_end` (src/repair.rs:1356)
  raises a request or a denial at once.
- The dirty-tree naming: `dirty_files` / `dirty_refusal`
  (crates/trees/src/cmd_checkout.rs:2699, :2715, wsp-605).
- The check-line parser (src/check.rs:162 `step_of`). It takes `name (time): cmd`,
  but only when the name is **one word** (`head_of`, :214). Any other label falls
  through, and the whole line becomes the command. That is the root of wsp-609
  and compounding-442.
- `$WSP_SEAT_KIND` (closing.rs:152, wsp-538) tells a process which kind of seat
  it is in.

So the change is mostly about **when** and **in what shape**. The gates move to
one verb that runs them all at once, against one file whose sections are fixed.
The file becomes the record.

## 2. The template

Every packet ends with the template for its seat's kind (§6). The seat writes it
to `$WSP_EXIT`, the path `closing::verdict_path` already gives (kept as
`$WSP_VERDICT` too, §3), with a heredoc in its own Bash call. Sections are fixed,
in this order, and each starts at the beginning of a line. Anything outside a
known section is refused (V0). That keeps the parser simple and the refusals
exact.

### 2.1 A member

```text
summary: <one line, under 100 characters: what is true now>

<the account: what you did, what you left, what the next reader needs.
 This is review's robustness-101 payload, unchanged.>

done-when:
- held: <the item, as the packet says it> — <where: a test name, file:line>
- not held: <the item> — <why>

checks:
- <label> (<time>): <command>

asks:
- governor: <a decision the list's governor can make>
- person: <a decision only the person can make>
- note: <something worth reading that asks nothing>

owed:
- <what is left, one per line>   (or the single word: nothing)
```

### 2.2 A verifier

```text
verdict: holds | blocks
summary: <one line>

<the verdict in prose>

done-when:
- held: <item> — <evidence you read or ran>
- not held: <item> — <what is wrong, file:line>

checked:
- <label> (<time>): <command> — pass | fail [— reran once at load N]

cause: work | check | tree | env — <one sentence>     (required on blocks)

asks:
owed:
- <what the member owes; becomes the `sent back:` line>   (required on blocks)
```

`cause:` exists because 66 of 73 blocks record none. Each value routes the
block somewhere different:

- `work`: the change is wrong or incomplete. The row is sent back, and `owed:` is
  what it owes (packet::owed prints it to the next seat).
- `check`: a check failed, and `checked:` names it. Sent back the same way.
- `tree`: commits are missing, or the tree is dirty. V1 should have stopped this
  at the member's submit. A `tree` block is filed as a defect against the gate as
  well as sent back.
- `env`: the verifier could not verify. Its tree was lost, the load was too high,
  or the tool is missing (449). **This is not a block of the work.** The row stays
  at review, the pass is recorded `could not verify`, and the run starts a new
  verifier. A member is never sent back on a verifier's environment.

### 2.3 A barrier check

The same shape as a verifier, with `verdict: go | hold | followup`, `checked:`
lines, and `cause:` required on `hold`. `followup` stays a flag to the person,
because the rows it names are not ones wsp can guess (closing.rs module doc).

### 2.4 A leg

A leg that is not the last has `summary:`, the account, `checks:` and `owed:`.
Its done-when is the leg order's, so `done-when:` is optional. Its `submit` is
`wsp leg done`, and the baton is the file. The `finish` leg submits as a member.

## 3. The verb

```text
wsp submit <row> [--from FILE|-]
```

- With no `--from`, it reads `$WSP_EXIT`. The kind comes from `$WSP_SEAT_KIND`,
  never from the file, so a member cannot submit a verdict. From a verifier seat,
  `<row>` is the member it read, as `verified` takes today.
- It runs every rule in §4 and prints **every** refusal at once, each naming its
  section, its line number and the text to write instead. It then exits 1 and
  moves nothing. A seat fixes its file and submits again. Nothing is half-taken.
- When the file is accepted, wsp does what the old verb did (review / verified /
  go-hold / leg done), records the exit file whole as the row's account, files
  each ask through its route, and ends the seat (§5).
- `--dry-run` validates and prints what it would do. The packet's template says
  to use it once, which costs one call and catches most refusals before the real
  submit.

### What stays, and as what

| Verb today | After |
|---|---|
| `wsp review <id> "…"` | **Alias.** From a run's seat it builds an exit from its words (summary = first line, account = the rest). It runs the same validator, and the missing structured sections are warnings while the brief changes (§10 D2). Once one list has run clean on `submit`, a run seat's bare `review` is refused, and the refusal prints the template. Outside a run (a person, an ad-hoc seat), it stays as it is today. |
| `wsp verified <id> --holds/--blocks` | **Alias** of `submit` for the verifier kind, with the word as `verdict:`. `--blocks` without a `cause:` is refused. |
| `wsp worklist go/hold` from a check seat | **Alias** of `submit` for the check kind. The person's and the primary's `go`/`hold` are unchanged: they are decisions, not exits. |
| `wsp leg done` | **Alias** of `submit` for a leg seat. |
| `wsp check --set` | **Stays**, for declaring checks mid-work. At the exit, `checks:` is what writes `## Checks`, through the same validator (V3), so the two cannot disagree. |
| `wsp flag`, `wsp ask` | **Stay**, mid-work: a hand raised while the seat keeps working, or a question it waits on. They are not exits. An exit's `asks:` covers the end. |
| `wsp block`, `wsp park` | **Stay** for a person or a governor. From a run seat, `block` is the exit with a `not held` line and a decision ask (§4 outcome). |

## 4. The validator

The rules run in order and all of them run. A refusal names the line.

- **V0. Shape.** Every section is known, in order. `summary:` is present.
  `done-when:` is present for a member (§2.4 for a leg). `verdict:` is present for
  a verifier or check. Refused: `line 14: "Tests:" is not a section of the exit —
  the sections are summary, done-when, checks, asks, owed`.
- **V1. The tree.** `git status --porcelain --untracked-files=all` is empty, or
  every path in it matches the project's **artefact list**. wsp restores those
  paths itself (tracked: `git checkout --` the path, in its own process; untracked:
  removed by exact path) and logs them by name on the row. That works where a
  seat's own `git checkout` was denied by auto mode (447), because wsp is not
  running under the classifier. Anything else dirty is refused with
  `dirty_refusal`'s words, which name each file, plus `commit it (wsp
  commit-help)`. The treecopy mismatch (`review_refusal`) runs here too.
- **V2. The commits.** The commit the exit is made at is HEAD, and HEAD is the
  branch's tip. Zero commits ahead of the trunk is accepted (a plan row, a row
  already landed) and recorded as `nothing to land`. A row whose `done-when:`
  cites a file path while nothing is ahead gets a warning. wsp-529's
  blocked-commit refusal runs here.
- **V3. Checks.** Each `checks:` / `checked:` line parses **strictly** as
  `label (time): command`. The label is everything before the last ` (` that is
  followed by a time and `): `, so it may be several words. The time is
  `Ns|Nm|Nh`, at most 30 m for a member. The command passes `sh -n`, and its first
  word is a builtin (`cd`, `env`), an executable on PATH, or a path inside the
  tree. A line that does not parse is refused with the shape to write. It is never
  run. **Scope:** a member's line may not be one of the project's barrier or end
  steps (`check::barrier_steps`, `check::end_steps`), and may not be a full-suite
  command the throttle already guards (`cargo nextest run --workspace`, `npm run
  check`). Refused: `line 9: "npm run check" is the barrier's full suite — name
  the narrow command for what you touched`. `full-suite: <why>` on its own line in
  `checks:` lets it through, logs the reason (wsp-389's `WSP_FULL_SUITE`), and
  shows it to the verifier. **The parser is one function**, used by the exit,
  `wsp check --set`, `wsp worklist group --stop` and the barrier runner. That is
  wsp-609's first two points.
- **V4. Done-when.** Each line opens with `held:` or `not held:` and carries a
  reason after ` — `. When the row has a `### Done when` (or `Done when:`)
  paragraph of bullets, the count must match and each line must start with its
  item's first words. Prose done-whens are not counted.
- **V5. Asks.** Each line is typed `governor:`, `person:` or `note:`. An untyped
  line is refused. A `governor:` or `person:` line that `waiting::Words::asks`'
  rule (src/waiting.rs:188) reads as asking nothing is **filed as a note and
  said so**, not refused. That is 439's case: a "ready" report never becomes a
  question. Each question goes through `waiting::route` (src/waiting.rs:363,
  wsp-608), so the seat that may close it is the one the barrier names.
- **V6. The account.** `summary:` is non-empty and under 100 characters. The prose
  body is non-empty, which is review's robustness-101 gate, kept.
- **V7. Owed.** `owed:` is present: `nothing`, or lines. Every `not held:` needs
  an `owed:` line, or a decision ask that it waits on. A verifier's `blocks` needs
  an `owed:` line, because that becomes the `sent back:` the next seat reads.

### The outcome is read from the file, not chosen

| The file says | A member's row | The seat |
|---|---|---|
| every item `held` | `review`; the verifier runs | ended (§5) |
| any `not held` and a `governor:`/`person:` ask | `blocked` on that ask, routed | ended; the answer reopens the row with the answer as its `sent back:` reason, so packet::owed briefs the next seat (§10 D1) |
| any `not held` and only `owed:` | `review` as partial; the verifier reads `owed:` | ended |

### Where the artefact list lives

It goes in the project file, `~/wsp/projects/<project>.md`, as a `## Artefacts`
section. It sits beside `## Checks`, which `check::project_steps` already reads
from the same file:

```markdown
## Artefacts
- docs/web-chrome/plugins/shots/*.png — rewritten by `npm run check` (compounding-456)
```

The globs are matched the way `check::matches` matches a rule's `on`. The project
files are Ed's, so this plan only proposes compounding's first line above. It does
not write it. `verifier_packet::is_run_output`'s hard-coded list
(src/verifier_packet.rs:38) answers a different question (what a verifier's packet
does not print), so it stays. It could later read the artefact list as well.

## 5. An accepted exit ends the seat

A1. On acceptance, wsp ends the seat in the same call, by its pid tree. It uses
ending.rs's confirmed despawn (`verifier_pids` before, `confirm_gone` after), and
the tree is kept. ending.rs's guards still apply: a governor seat is never ended,
and neither is a seat a person has typed to since. A seat with a turn still in
flight is ended when that turn stops. It does not wait for the next tick.

The member case conflicts with an existing rule. ending.rs (its module doc,
wsp-193) keeps a member's seat at review until its verdict holds, because "a
blocks verdict is answered from that context". This plan recommends ending the
seat at submit anyway:

- The ~15 seats left up overnight were exactly these, and with 13 idle panes
  holding 2.8 GB the machine thrashed (overnight.log 14:23).
- A send-back already restarts most rows in a fresh seat. cycle.log shows 8
  "starting … again: it was sent back and nobody is on it" lines in the window,
  and packet::owed exists for that case.
- With `cause:` and `owed:` required, the send-back carries what the context
  used to.

The alternative is §10 D1.

This closes the remainder of wsp-385, which already ends check and verifier seats
on their verdict, by making the end part of the verb rather than a later tick.

## 6. The packet carries the template

`packet::render`'s tail (src/packet.rs:418) gets the template for the seat's kind,
after the brief and before the files. Its `done-when:` lines come pre-filled from
the row's `### Done when` bullets, as `- held: <item> — ` for the seat to finish
or flip. The verifier packet (src/verifier_packet.rs:139) and the barrier check's
order (`cycle::packet_text`, src/cycle.rs:3433) end with theirs. The words
`how_to_close` / `how_to_close_verdict` (closing.rs:118, :237) give become one
line: write `$WSP_EXIT` with a heredoc in one call, `wsp submit <row>` in the
next. The template costs about 600 bytes against the packet's 16 kB cap.

## 7. A turn that ends without a submit

S1. **On the seat's `Stop`, within one tick.** The row is still `doing` (or the
pass or check is still running), no exit has been accepted since the seat
started, and the turn did not end wanting somebody. Then wsp tells the seat, once
per stop: either `no exit at $WSP_EXIT`, or `the exit at $WSP_EXIT is refused:`
followed by V0–V7's lines run in dry-run against the file as it stands. Then the
one command. This is the branch beside wsp-606's `raise_turn_end`
(src/repair.rs:1206), in the same reading of `TurnEnd`. A turn that **does** want
somebody (a request in its last words, or a denied call) is still raised at once,
to the governor when one is seated and otherwise to the person, unchanged. Today
an idle member is told after `STALLED_AFTER` (repair.rs:84, 10 min) and a leg or
verifier after `TELL_AFTER` (closing.rs:59, 3 min). Both become "on the stop".
The end-and-restart windows after the tell stay as they are.

S2. **A seat at an on-screen prompt.** An AskUserQuestion menu or a permission
prompt, in a verifier, check or member seat of a run, is raised at once through
`waiting::route`, with the prompt's text. It does not wait out a window, and it
covers 449. wsp-608 already lets such a prompt hold the barrier. S2 makes sure
somebody is told.

S3. **Closing for the seat stays as the backstop.** A seat still idle a window
after the reminder, with a parseable exit written, is submitted by wsp
(`close_member` / `settle_*` today, reading the exit file instead of the verdict
file). A file that does not validate is a flag to the person with the refusal
lines, as `close_member` flags today.

## 8. How it absorbs the open rows

- **wsp-606** (landed e37d6b0): its raise is S1's "wants somebody" branch,
  unchanged. Its Stop-measured clock becomes the reminder's trigger.
- **wsp-608** (landed d1b2c40): V5 routes every ask through `waiting::route`. A
  `note:` is never a question, and an exit never re-files a review as one.
- **wsp-609** (todo, high): V3's parser **is** wsp-609's parser, and it should be
  built once. Recommendation: wsp-609 lands first, as filed, on its own list. Row
  E1 (§9) takes its parser as the exit's check grammar, and widens it only
  if wsp-609 left labels as one word. wsp-609's hung-step report stays wsp-609's.
- **wsp-605** (review): `dirty_files` / `dirty_refusal` are V1's words. V1 makes a
  hold whose land is owed rare. wsp-605's land-owed retry stays as the backstop.
- **wsp-385** (done): §5 extends it to members, at the verb.
- **compounding-456**: the shots that `npm run check` rewrites are compounding's
  first `## Artefacts` line (§4), and that line is Ed's to write.

### Relative to wsp-483 (the state core)

The plan recommends building this **standalone now, shaped as the first piece of
wsp-483**. The state core's executor protocol (docs/state-core.md §5.1) has a
`report` message that carries spend, last word, transcript ref and copy-back
manifest. It also has a seat machine (`working ⇄ idle`, `ending`, `gone`) and a
row machine. The exit is the payload of a new `submit` message (E → S), and the
**validator is a guard on the row machine's `doing → review` transition** and on
the pass machine's `running → holds|blocks`. To keep that cheap later, E1 defines
the exit as one Rust type with a serde JSON round trip as well as its text form.
When the server exists it stores that type, and the CLI's text file is one client
of it. Nothing here waits on the server. The state core is a draft, and the
stalls are happening now.

## 9. The build, as rows on this list

Groups 2–8 of `exit-packet`, one row each, in order. Each row's done-when is
tests in its own module, run with that module's filter. The full suite is the
barrier's.

| Group | Row | What | Rooted paths |
|---|---|---|---|
| 2 | E1 contract | The exit type for member, verifier, check and leg: text parse and render, JSON round trip, the strict check-line grammar (shared with wsp-609), typed asks, the outcome table. No verb, no IO. | src/exit.rs (new), src/check.rs (`step_of`/`head_of` to the shared grammar), src/main.rs (mod) |
| 3 | E2 validator and `wsp submit` for members | V0–V7, `## Artefacts` read and restored by wsp, every refusal at once, `--dry-run`, acceptance through review's existing path, asks through `waiting::route`. `review` from a run seat becomes the alias | src/cmd_submit.rs (new), src/cmd_task.rs (`review`'s core, shared), src/check.rs (project artefacts), crates/trees/src/cmd_checkout.rs (`dirty_files`, restore by path), src/agent_commands.rs, crates/base/src/base/args.rs |
| 4 | E3 `submit` for verifiers, checks and legs | `verdict:`, `checked:`, `cause:` required on blocks/hold, `env` re-verifies instead of sending back. `verified`, a check seat's `go`/`hold`, and `leg done` become aliases | src/verification.rs (`verified_as`), src/closing.rs (`parse_verdict`, `settle_verifier`, `settle_check`, `settle`), src/cycle.rs, src/legs.rs |
| 5 | E4 an accepted exit ends the seat | Member, verifier and check seats ended at acceptance by pid tree, tree kept, ending.rs's guards kept. The cap count drops in the same call | src/ending.rs, src/closing.rs, src/cmd_submit.rs |
| 6 | E5 the Stop-hook reminder | S1–S3: the reminder on the stop with dry-run refusals, the on-screen prompt raised at once, closing-for-the-seat reading the exit file | src/repair.rs (Idle arm), src/closing.rs (`tick_at`), src/waiting.rs |
| 7 | E6 the packet carries the template | `packet::render`'s tail per kind, done-when pre-filled, `$WSP_EXIT` set at spawn, the verifier packet and the check order end with theirs, `how_to_close*` reworded | src/packet.rs, src/verifier_packet.rs, src/cmd_spawn.rs, src/cycle.rs (`packet_text`), src/closing.rs |
| 8 | E7 docs | This file becomes the reference for what was built. `wsp help` for `submit`. The project handbook's "What an agent is handed". docs/legs.md's close. The brief text of §11 goes to Ed as a flag, not an edit | docs/exit-packet.md, docs/legs.md, src/agent_commands.rs (help), crates/store/src/verbs.rs |

E1 → E2 → E3 is a chain: each uses the one before. E4 and E5 both edit
closing.rs, so they go in separate groups. E6 changes what every seat reads, so
it goes last among the code rows. Otherwise the brief would point at a verb the
seats it starts cannot yet run.

The rebase leftovers in §1 (7 send-backs on "there is already a rebase-merge
directory") are not exit work. They are named in the follow-up note to the
governor and not filed here.

## 10. Decisions for Ed

1. **D1: end the member's seat at submit, or keep it until the verdict?** The
   recommendation is to end it (§5): places, memory, and a send-back that now
   carries its cause. The alternative is wsp-385's other half: keep the seat but
   do not count it against the cap, at the cost of the memory it holds.
2. **D2: how long does bare `review` from a run seat stay a warning?**
   Recommended: until one full list has run on `submit` with no stall at the exit,
   then refuse.
3. **D3: should a `not held` with a decision ask end the seat?** Recommended: yes,
   with the answer reopening the row as its `sent back:` reason. The alternative is
   to keep the seat up and waiting, as `wsp ask --wait` does. That holds a place
   for as long as the answer takes, and 439's answer took 8½ h.
4. **D4: the full-suite override.** Should `full-suite: <why>` be allowed in a
   member's `checks:` at all, or only through `WSP_FULL_SUITE` while it runs?
5. **D5: the artefact lists** are yours to write in ~/wsp/projects/*.md. Is
   compounding's first line (§4) right?

## 11. Proposed brief wording (for ~/wsp/agents.md; Ed edits it)

Replace the line "Finished? `wsp review`. …" with:

> - Finished, or stopping for any reason? Your packet ends with the exit. Write it
>   to `$WSP_EXIT` with a heredoc in one call, then run `wsp submit <id>` as a
>   call of its own. If it refuses, it names the line to fix: fix it and submit
>   again. An accepted exit ends your seat. `done` belongs to the person you are
>   working for.
> - Never end a turn on a question in prose. Nobody reads your screen. A question
>   at the end goes in the exit's `asks:` (`governor:` or `person:`). A question
>   mid-work is `wsp ask`.

In "Need a decision?", replace "`wsp block <id> \"the question\"`, say so, and
stop" with "put it in the exit as a `not held:` line and a `governor:` or
`person:` ask, and submit", and keep the rest.
