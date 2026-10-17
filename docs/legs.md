# Legs: a row run as a chain of agents

`wsp-287`, 2026-10-17. Nothing here is built yet. The build rows are at the end.

## The decision

A row can run as a chain of **legs**. Each leg is a seat of its own, with its
own kind, model and effort and a fresh context, working in the row's one
worktree. It gets a small **baton** from the leg before it, ends at a commit,
and is recorded as one attempt on the row. A leg has no ticket. The chain is a
`## Legs` section on the row, written the way `## Verification` is (`wsp-188`):
the entry is the record, and the only one.

This answers the row's first question (roles inside a row, or more rows with
`after`) the way Ed did on 2026-10-17: roles inside the row, with no ticket per
stage. It is not the overseer dispatching subagents inside one session. Why not
is under *Not this* below.

## Why

The flight comparison is in the row's log (2026-10-15). In short: a Sonnet
design row ahead of an Opus build cost $21–57 a pair, against $12–57 for
unsplit Opus builds. Every builder read its 16–21k-char plan at call 1 and
then read the tree anyway (94–158 read calls). Editing a file needs the file,
so a plan cannot replace the reading. The costs come from two places, and
legs address both:

- **Context that only grows.** Cache reads are ~80% of a session's cost, and
  each turn re-reads everything before it. A session of T turns that grows
  g tokens a turn from a start S₀ re-reads about T·(S₀ + gT/2). Split into k
  legs, where each leg re-reads R tokens of the tree to get its bearings, it
  re-reads about T·(S₀ + gT/(2k) + R(k−1)/k). The saving is
  **T·(1 − 1/k)·(gT/2 − R)**. That is positive once a single session would grow
  by more than 2R.
- **Opus doing the volume.** Opus input costs 2.5× Sonnet's (`cmd_burn::PRICES`,
  $5 against $2 a million). A leg that reads or edits to a plan does not need
  Opus. A leg that decides does, and it can do that from a baton and a few
  targeted reads.

The measured inputs: g ≈ 1.7k tokens a turn (compound-476 went from 43k to 481k
over 254 calls, and 512 reached 491k). R ≈ 60–80k, which is what the flight
builders read before their first edit. Break-even growth is therefore
2R ≈ 120–160k, or **about 70–95 turns**. That fits the first guess of ~80. A
row shaped like 476, run as four legs with one on Opus and three on Sonnet,
comes out at about 38M cache-read tokens and ~$10, against 66M and ~$33. That
figure is an estimate. The trial row exists to replace it with a measurement.

Below break-even, legs cost money. A leg's start writes ~43k to the cache
($0.27 on Opus, $0.11 on Sonnet), and it pays its R again.

## Names

The list is called `substages`, but `stage` is taken: `src/stage.rs` is the
caption word (`wsp-242`). `handoff` is also taken: it is an agent moving its
claim to another task (`cmd_agent::hand_off`). So:

- **leg**: one agent's part of a row. `## Legs` and `wsp leg …`.
- **baton**: what a leg leaves for the next one.

## The record

```text
## Legs
- scout · claude sonnet medium · done · pane cpd-31 · ended
  order: Read the three call sites of … and say which layer owns …
  baton: three layers: store (src/legs.rs new), spawn (cmd_spawn.rs:2274–2330), …
- plan · claude opus high · done · pane cpd-34 · ended
- build-store · claude sonnet medium · running · pane cpd-36
- build-spawn · claude sonnet medium · pending
- finish · claude sonnet medium · pending
```

- One entry per leg, in run order. Each entry has a name, a policy (a
  `model::Policy` line), a state (`pending | running | done | skipped`), and
  the pane and ending clauses that `verification::Pass` already has. The order
  and the baton are indented under the entry, each capped at 3,000 chars.
  Above the cap the text is refused, never truncated.
- **No cost here.** A leg's spend is its attempt's release line (see
  *Measured*). A second copy would be two records of one fact, which is how
  wsp-148 ended up with five rows.
- A leg with no policy runs on the member's line (`agent <member>:`, or else
  the group's line).
- Opening a leg (`running`) is written under the store lock **before** its seat
  is spawned, so it is the idempotence key, as a running pass is. As with
  verification bookkeeping, it does not move `updated`.

**Who may change it:**

- A person or the row's governor may add, reorder, edit or skip any
  `pending` leg at any time: `wsp leg add <task> <name> [--agent "<policy>"]
  --order FILE [--before <leg>]`, then `wsp leg skip <task> <name> "why"`.
- The seated leg may add or edit pending legs **after itself**. This is how a
  plan leg sets out the build legs, and how a build leg that hits a decision
  calls in an overseer: it adds a `decide` leg on Opus whose order is the
  question, then a leg to carry on.
- Nobody edits a `running` or `done` leg.
- **The cap is 8 legs**, counting rework. A row that needs more should be
  split into rows.

## A leg is a seat

`wsp spawn <task> --agent --leg <name>`, which the run normally types for you.
Everything that already handles a seat applies unchanged:

- **Claim.** The leg claims the row, so the session hook, `wsp brief`,
  `wsp say` and `review` all work as they do today. The claim and release
  lines each gain a clause, `· leg build-store 3/5`. This makes every leg one
  attempt, and `cmd_attempts` already records the tier, turns and spend of an
  attempt.
- **Tier.** The tier is the leg's policy, checked with `Kind::tier` before
  anything is opened, as a member's is.
- **Fresh context.** Every leg gets a new seat and a new session. It is not a
  `/clear` in the same pane: a clear cannot change the kind or the model, and
  only some kinds have a measured `clear_command`.
- **cwd.** The row's one worktree. Its branch collects every leg's commits.
- **Label.** `wsp-287 · build-store 3/5`, in the same form as
  `t-1 · verifying`.

**Ending a leg:** `wsp leg done <task> --from FILE --caption "…"`. The command:

1. **Refuses if the tree has uncommitted tracked changes.** Every leg boundary
   is a commit. That is what lets the next leg trust `git log` and `git diff`
   over the baton, and it gives the run the "reset at each commit" lever the
   flight comparison pointed at.
2. Refuses a baton over 3,000 chars or a missing caption. The caption is the
   tail `stage.rs` shows after the word.
3. Writes the baton, marks the leg `done`, and releases the claim. The release
   line carries the leg's spend.
4. Pokes the run.

**`wsp review` from any leg ends the chain.** The remaining pending legs are
written `skipped · ended by review`. A scout that finds nothing to do should
not have to walk the chain. **`wsp block` from a leg** blocks the row, as it
does today. A leg that finds the baton too thin to work from blocks rather
than re-deriving it. That keeps the next leg as the check on the previous one.

## The baton, and what a leg reads

The flight plans ran to 16–21k chars, and the builders read the tree anyway.
So a baton does not try to replace reading. It aims the reading. The
suggested shape (the cap is enforced, the shape is not):

- **Done**: the commits, and what they changed, one line each.
- **Where**: `path:line–line` for what the next leg needs to read. Pointers,
  not contents.
- **Decided**: what the next leg must not reopen.
- **Left**: anything this leg could not settle.

A leg's brief is today's brief plus a bounded block of about 8k chars:

- `leg 3 of 5 (build-store) on wsp-287`
- the leg table, one line per leg (name, tier, state, caption)
- its own order
- the previous leg's baton in full
- earlier batons as their captions only, with `wsp leg show <task> <name>` to
  read one whole

A plan leg's split of the work goes into the **orders** of the legs it adds,
each capped at 3,000 chars. No single document carries the whole plan, and
each builder gets only its own share.

## The run

The same levels-not-edges rule `cycle.rs` follows. Nothing new remembers
anything.

- **Starting.** `cycle::step` reads `## Legs` on every `doing` member. If one
  has no running leg, a pending one, and no live claim, the step opens the
  next leg and spawns it under the room cap (`crate::room`), as step 1 spawns
  members. A staged member's first leg is how it starts.
- **Ending.** The tick ends a `done` leg's seat once, the way `end_passes`
  ends a verifier, and records the outcome on the leg's entry.
- **Outside a list.** A row someone staged and spawned by hand has no run to
  step it. `wsp leg done` launches the next leg itself, detached, as `poke`
  does. The daemon tick's repair catches a lost launch in both cases.
- **Must not double-start.** The existing "doing with no seat" repairs
  (`wedged`, `sent_back_unseated`) must leave a staged row to the leg path.
  If they did not, they would spawn a whole-row worker beside the chain.
- **Verifier.** Unchanged. It runs once, on the row, after the last leg's
  review and land, and checks the row against its overview. The roles are
  the means and get no verification of their own. Its order gains one line:
  the row ran in legs, here is the table, and a verdict that blocks names the
  leg whose work fails.
- **Send-back.** A blocking verdict appends a `rework` leg. It runs on the
  policy of the leg the verdict names, or the last build leg if it names
  none, and its order is the verdict. The member goes back to `doing` and the
  step starts that leg. A row that has hit the cap goes to its governor.
- **Barrier.** Unchanged. It reads `wsp show`, which renders the leg table.

## Measured

The row asked how `wsp burn` can report per row and per role. Every number
needed is already written somewhere except two, and those two clauses are
the whole addition:

- **The leg clause** on the claim and release lines (above).
- **The spend clause on a verification pass**, written when its seat ends,
  using the same `Ran` the release line uses. The flight comparison counted
  the verifier's cost, and today only `wsp burn`'s per-seat tally sees it.
- **A `peak` clause on `Ran`**: the largest context any turn of the attempt
  read. Peak was the flight's fourth column, and it is the number staging is
  meant to bring down.

`wsp attempts <row> --cost` (or `wsp burn --row`; the build row picks the name)
prints one table per row: each attempt grouped by leg, then each verifier
pass, with tier, turns, peak and dollars, and a total. `--list <list>` does
this for every row of a worklist. With that, the flight comparison is one
command instead of an afternoon of transcript arithmetic. It also works for
rows that are not staged, so the baseline can start building up before legs
exist.

## Seen

- **wip and census.** A leg's seat is a claim, so it shows up already. The
  claim entry in `wip --json` gains `leg: {name, index, of}`, and the text line
  reads `wsp-287 build-store 3/5`.
- **Caption.** `stage.rs` keeps its word (`doing`). The tail is the newest
  leg's caption, so a row reads `doing · scout: three layers, plan next`. No
  new word, and nothing derived that a person did not write.
- **compound.** compound reads the seat label and `wip --json`, so it gets the
  leg in both places with nothing changed in compound. Drawing the leg table
  in the row's detail would be compound's own row.
- **`wsp show`.** Renders `## Legs` like `## Verification`: one line a leg,
  with order and baton folded to their first line, and `--legs` prints them
  whole.

## When to stage

This is the rule for the handbook, kept to what can be checked when the list
is written:

- **Stage a row** when it is expected to run past ~80 Opus turns, or when it
  crosses more than one layer (each layer becomes a build leg).
  Expected-past-80 in practice means it reads more than it edits (476 made
  ~5 edits in 254 calls), touches more than one module family, or has a
  plan that names more than one step under *Order*.
- **The default chain:** `scout` (Sonnet: reads, and writes a baton of
  pointers), then `plan` (Opus: decides from the baton plus targeted reads,
  and adds the build legs), then `build-<layer>` × N (Sonnet: edits, runs the
  loop, commits), then `finish` (Sonnet: the full test, rebase, land,
  review).
- **Do not stage** a row under ~80 turns that sits in one layer. The extra
  starts and re-reads cost more than they save.
- The threshold is a calculation from four rows. The trial row is where it
  gets measured and either confirmed or moved.

## Not this

- **The overseer dispatching subagents in one session** (Claude Code's Agent
  tool with a cheaper model). It works today and stays open to any leg as a
  tactic of its own, but it is not the mechanism. Three reasons. A subagent's
  spend is a sidechain, which `cmd_attempts` skips by design, so the
  comparison could not be measured. It is Claude-only, while opencode and pi
  are kinds wsp runs. And the overseer's own session still lasts the whole
  row, and that growth is the cost.
- **More rows with `after`.** That is the flight's design-then-build split,
  measured, and it saved nothing. It also adds a ticket, a verifier and a
  barrier slot per stage. Ed asked for no ticket per stage.
- **A verifier per leg.** The next leg reads the baton and blocks if it is
  thin, and the row's verifier reads the outcome. A per-leg verifier would
  pay a session start to check something that is checked twice already.

## Build rows

Each row is filed `-p wsp`, has a **Done when**, and uses `wsp after` as shown.
1 and 2 can share a group. 4 and 5 can share a group.

1. **legs-measure: a row's cost is one command.** Adds the spend clause on
   verification passes, `peak` on `Ran`, the leg clause read by `cmd_attempts`,
   and the per-row and per-list cost table. *Done when* `wsp attempts
   compound-512 --cost` (or the chosen name) prints worker, rework and
   verifier spend with tier, turns and peak. Tested on captured log lines.
   *After:* nothing. It measures unstaged rows too.
2. **legs-record: `## Legs` on a row.** Adds the parse and write module
   (`legs.rs`, modelled on `verification.rs`), `wsp leg add | skip | show`,
   the authority rules, the caps (8 legs, 3,000-char order and baton), and
   `wsp show` rendering. *Done when* every rule in *The record* has a test
   that builds exactly the record it refuses or accepts. *After:* nothing.
3. **legs-seat: a leg is a seat.** Adds `spawn --leg`, the leg clause on
   claim and release, the leg brief block, `wsp leg done` (clean tree, caps,
   caption, release, poke), and `review` skipping the rest. *Done when* a
   sandboxed (`wsp sandbox --fake`) staged row goes scout → done →
   (spawn of the next leg by hand) → review, and each attempt line carries
   its leg. *After:* 2.
4. **legs-run: the run steps the chain.** Covers `cycle::step` starting legs
   under the room cap, the tick ending done legs' seats, the detached next
   leg outside a list, the `wedged` and `sent_back_unseated` exclusions, the
   verifier's order line, and blocks → `rework` leg. *Done when* a
   `wsp sandbox --fake --stage` list with one staged member runs every leg
   to verified with no hand step, a blocking verdict produces exactly one
   rework leg, and a second advance starts nothing twice. *After:* 3.
5. **legs-seen: wip, captions, label, handbook.** Adds the seat label, the
   `leg` field in `wip --json`, the caption tail, and the *When to stage*
   rule in wsp's handbook. *Done when* `wsp wip` and `wsp wip --json` show
   the leg of a sandboxed staged row, and the handbook carries the rule.
   *After:* 3.
6. **legs-trial: the comparison, measured.** Runs two or three large rows
   (expected past 80 turns) staged on the default chain, compares them with
   1's table against unstaged Opus rows of similar scope, and records a
   decision that confirms or moves the threshold. *Done when* that decision
   is recorded with the table behind it. *After:* 1, 4, 5.
