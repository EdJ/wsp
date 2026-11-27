# Pieces: a larger work in defined parts, inside one row

`wsp-501`, 2026-11-08. A design, nothing built. Ed confirmed ids (1) and the one
verifier (4) on 2026-11-08. The rest settles the primary seat's proposals, and
says where it departs from them. The build rows are at the end.

## The decision

A row can carry **pieces**: the parts of a larger work, each with its own id,
overview, done-when and check lines. Pieces run in **batches**. The pieces of
one batch run side by side, each in its own tree off the row's branch. A batch
is merged into the row's branch before the next one opens. When the last batch
is merged and green, the row goes to review and **one verifier** judges the
row, reading each piece's done-when and check results. When the row lands, its
pieces fold into its log. A piece has no store row, no list membership, no
packet of its own, no verifier and no barrier slot.

Pieces sit between the two things wsp already has:

| | legs | pieces | sub-tasks |
|---|---|---|---|
| for | coupled work: one tree, one built artefact or one line of reasoning | a larger work in separable parts | separate deliverables |
| id | none (a name on the row) | `<row>-<n>` | `<project>-<n>` |
| own done-when and checks | no | yes | yes |
| verdict | the row's | the row's, read piece by piece | its own |
| shape | a graph (`after:`), one last leg | batches, in order | list groups and `after` |
| added while running | by a seated leg, downstream of itself | to a batch not yet open, by anyone with the right | a list edit, write-ahead |
| outlives the row | no | no (folds into its log, or is promoted) | yes |

In short: coupled work under one verdict uses legs; a larger work in defined
pieces uses pieces; work that needs its own verdict, review or life in the
backlog uses sub-tasks.

**The four questions, answered.** Each has its section.

1. **A piece holds no claim.** It is recorded on its entry and its pane is
   bound to the row, as a leg is (*A piece is a seat*).
2. **The barrier sees one member.** A row with pieces is one member of its
   group, with one verdict and one landing. Its pieces count against the
   machine's agent cap and the row's own cap, not the group's `xN` (*The
   barrier*).
3. **The size check offers all three.** It names what tripped and accepts
   legs, pieces, sub-tasks or `--whole --why`. Its message says which fits
   which shape (*Choosing*).
4. **A piece saves about $0.10–0.14 net and a place for ~1.5 minutes against
   a row**, nearly all of it the verifier seat. On react-pass-3 group 2 (15
   rows) that is about a quarter of the group's spend. It does not save wall
   time (*Cost*).

## Why

What a row costs beyond its seat, measured on the two React passes
(`wsp worklist cost react-pass-3|react-pass-2 --json`, read 2026-11-08):

| list | members | total | workers | verifiers | governor | barrier | rework |
|---|---|---|---|---|---|---|---|
| react-pass-3 | 35 | $28.67 | $13.72 (48%) | 40 seats, $6.72 (23%) | $3.46 | $1.10 | $3.68 |
| react-pass-2 | 24 | $18.92 | $8.99 (48%) | 29 seats, $5.02 (27%) | $3.05 | $1.31 | $0.56 |

**A verifier is mostly a fixed cost.** Over every list in the store (104 lists,
606 members with a measured worker and verifier), the verifier's median spend
barely moves with the size of the work it judges:

| worker turns | members | verifier median | verifier clock | worker median |
|---|---|---|---|---|
| 0–20 | 167 | $0.13 | 80s | $0.18 |
| 20–40 | 169 | $0.16 | 97s | $0.51 |
| 40–80 | 162 | $0.19 | 122s | $1.26 |
| 80–150 | 79 | $0.20 | 240s | $3.11 |
| 150+ | 29 | $0.31 | 539s | $7.94 |

So a larger work split into fifteen rows pays fifteen verifier starts to judge
what one verifier could judge for about twice the price of one. Small rows pay
the most: on rows under 20 turns the verifier costs 70% of the worker.

The rest of a row's cost is bookkeeping, and it is what Ed asked pieces to
save: a store row and its id that outlive the work (react-pass-3 left 35 rows
in compounding, which holds 204), list membership, a generated packet, a
verifier seat, and the barrier's and governor's attention per member. It also
fixes the gap that stalled react-pass-3 overnight (`wsp-495`): a running group
refuses new members, so nothing could add work to it. Pieces can be added to
a batch that has not opened, with no list edit.

**What it does not change.** A piece is a seat with a fresh context, so it
pays the start and the reading a row does. The planner still writes an
overview, a done-when and check lines per piece. compounding-163 filed 35 rows
for $4.08 in 72 turns, and filing them as pieces would cost about the same,
less the per-row `wsp add`, `check --set` and `worklist add` calls (*The
record*: one file can add a batch).

## Ids

**`<row>-<n>`**, so piece 2 of `compounding-199` is `compounding-199-2` (Ed,
2026-11-08: keep the second hyphen). `n` counts from 1 within the row and is
never reused, so a dropped, promoted or reworked piece keeps its number and a
new piece takes the next one.

- **The parse.** An id `<a>-<n>-<m>` with `n` and `m` all digits is piece `m`
  of row `<a>-<n>`. This is unambiguous only while no project's code ends in
  `-<digits>`. Measured 2026-11-08: no project id or code in the store does,
  and no task or archived task id has two numeric parts (the dated
  `t-YYMMDD-NNN` scheme is gone: no `t-` ids remain). `wsp project add`
  refuses a slug that ends in `-<digits>`, and so does anything that sets a
  project's `code` (`cmd_migrate`). The refusal names the reason.
- **The resolver.** `Store::resolve_task` stays about rows. A new
  `pieces::addressed(store, needle)` tries the piece shape first and returns
  the row and the piece, or the row and the log line the piece folded into
  once the row has landed. The verbs that take a piece call it before
  `task_or_why` (*Seen and told*). `looks_like_id` already reads a piece id
  as id-shaped, so a piece id never falls through to a title search.
- **Trees and branches** are named by the piece id: `.worktrees/wsp-501-2` on
  branch `wsp-501-2`. A row's tree is named after its id, and so is a
  piece's. `cmd_checkout`'s `stale` and `strays` resolve the name through
  `pieces::addressed`, so a piece tree is the row's, never an orphan. Legs
  keep `<row>.<leg>`, and a piece id carries no `.`, so the two names cannot
  meet.

## The record

```text
## Pieces
cap: 3
- 1 · contract types · batch 1 · done · pane cpd-31 · tree wsp-501-1 · ended
  overview: Types and a fixture each for runstate, worklists, taskpage …
  done-when: each module's pass3 test green; the fixtures read back
  check: tsc (2m): cd spikes/web-chrome && npx tsc --noEmit
  check: contract (3m): cd spikes/web-chrome && npx vitest run src/contract
  checked: green at 52c768d
  note: Done: 52c768d types+fixtures. Where: src/contract/*.ts. Left: …
  caption: types and fixtures read back
- 2 · board component · batch 2 · running · pane cpd-36 · tree wsp-501-2
  overview: …
  done-when: …
  check: board (1m): cd spikes/web-chrome && npx vitest run src/board
- 3 · rail places · batch 2 · claude opus high · pending
  why: decides between the rail's two layouts the parity list leaves open
  overview: …
```

- **One entry per piece**, in the order added: number, title (≤ 80 chars),
  batch, an optional policy, state, and the pane, tree and ending clauses a
  leg has. Indented under it: `overview:`, `done-when:`, one `check:` line
  per check (the `## Checks` line format, label first), and, as the piece
  runs, `checked:`, `note:` and `caption:`.
- **States**: `pending | running | done | blocked | dropped | promoted`. A
  `dropped` or `promoted` entry keeps a `why:` or `promoted: <id>` line.
- **The tier is `legs::DEFAULT_AGENT` unless the piece says otherwise, and
  says why**: the legs rule, the same constant and the same two refusals
  (another tier with no `why:`, a `why:` on a default piece). The group's and
  member's `agent` lines do not reach a piece.
- **`cap: N`** is how many of the row's pieces run at once, default 3. The
  machine's cap (`crate::room`) applies on top. Batches are meant to be small
  and a parent of fifteen pieces should not take every place on the machine
  from the rest of its list. (Legs have no per-row cap; pieces do, because
  Ed's proposal asks for one and a pieces row is wider by design.)
- **Caps on text.** Overview 3,000 chars, done-when 600, a note 1,500. A
  batch's notes together are read whole by the next batch up to 6,000 chars,
  and as captions beyond that, as batons are. **At most 20 pieces** in a
  row, counting dropped, promoted and rework. A work wider than that is rows.
- **Pieces or legs, not both, at the top of a row.** A write that would give
  a row both sections is refused, naming the other.
- **Write-ahead.** A piece is added to a batch that has not opened, or to a
  new batch after the last. A batch has opened once any of its pieces has
  run. Nothing edits a running or done piece. This is the worklist's rule,
  for the same reason: a batch's tree layout is fixed when it opens.
- **Who may write.** A person, the primary seat, the row's governor and a
  planner may add, edit, drop or promote any pending piece of a row at
  `todo` or `doing`. A seat working the row (it holds the claim) may stage
  pieces and release, as a seat stages legs (`docs/legs.md`, *Staging a row
  already running*). **A seated piece may add pieces to later batches and
  edit only pieces it added**: it is how a piece that finds itself too big,
  or finds a prerequisite inside the row's repo, hands on the rest.
- **The verbs.** `wsp piece add <row> "title" --batch N|next --overview FILE
  --done-when "…" [--check "label (time): cmd"]… [--agent "…" --why "…"]`,
  and `wsp piece add <row> --from FILE` for several at once (one `### title`
  block each, with `batch:`, `done-when:` and `check:` lines), which is how a
  planner files a batch in one call. `wsp piece show <id>`, `drop <id> "why"`,
  `promote <id>`, `reset <id> "why"`, `done`, and `wsp piece cap <row> N`.

## A piece is a seat

`wsp spawn <row> --agent --piece <n>`, which the run normally types.

**A piece holds no claim.** This is the first of the row's questions, and the
answer is the one legs reached (`docs/legs.md`, *A leg is a seat*). A row has
one claim (`claims.json` is keyed by task) and `claim` refuses a second live
holder, so the pieces of one batch could not each hold it. Keying claims by
piece id instead would make every claim reader (`live_holders`, release,
`reap`, `wedged`, `sent_back_unseated`) learn a second kind of claim, and every
one of those has been a source of double starts before. A leg already has
what a claim gave: the seat on its entry, written before the agent starts, as
the idempotence key; the pane bound to the row, so `task_in_hand` finds the
row; start and end lines in the log that `cmd_attempts` reads as attempts;
and the start-miss backoff (`starts.json`, `untaken`, `legs::reset`). A piece
takes all of it, and the binding adds `"piece": n` beside `"leg"`, so a
command from a piece's pane knows which piece it is.

- **Lines.** `piece 2 started by pane cpd-36 · spawned at sonnet/medium` and
  `piece 2 ended after 12m · ran sonnet-5-5/medium · 23 turns · … · peak …`.
  `cmd_attempts` pairs them by piece number, as it pairs legs by name.
- **The brief.** The row's packet once, then the pieces table (one line a
  piece: number, title, batch, state, caption), then this piece's entry whole,
  then the notes of the batch before it, under the 6,000-char total. A piece
  does not get another piece's overview, only its title and caption.
- **Ending: `wsp piece done --note FILE --caption "…"`**, from the piece's
  own pane. It refuses a tree with uncommitted tracked changes, as `leg done`
  does. It runs the piece's `check:` lines in the piece's tree and refuses on
  red, naming the step (the piece is not done while its own done-when's
  checks fail). On green it writes `checked: green at <sha>`, the note and
  caption, marks the piece `done`, writes the end line and pokes the run.
  `wsp review` from a piece's pane is `piece done`, with the account as the
  note. `wsp submit` from a piece's pane is refused, naming `piece done`: its
  exit is the whole row's. A piece never moves the row's status.
- **`wsp block` from a piece blocks the piece, not the row.** The pieces are
  separable by definition, so its siblings carry on. The row's governor is
  told once, with the piece's id, and the batch cannot close until the piece
  is answered (`wsp tell <piece-id>`, then `wsp piece reset`), dropped or
  promoted. This is where pieces differ from legs, whose block blocks the row.

## Batches and trees

**A batch of one stands in the row's tree. Each piece of a wider batch gets a
tree of its own**, `<trunk>/.worktrees/<piece-id>`, cut from the row's
branch at its tip when the piece opens. Which is which is read off the batch,
which is fixed once it opens (write-ahead), so it never depends on timing.
The reasons are legs' (*Trees* in `docs/legs.md`): the commit boundary, one
build per tree, files observed not declared, tests that see a neighbour's
edits. A piece tree costs what a member's tree costs today, since every row
already gets one.

**A batch closes when every piece in it is `done`, `dropped` or
`promoted`.** Closing is the run's, with no seat, and reuses `leg_tree`:

1. **Merge.** Each piece branch is merged into the row's branch, in the row's
   tree, in piece order, with `git merge --no-edit` (`leg_tree::join`'s
   rules: nobody standing in the row's tree, abort on a conflict so the tree
   stays clean at a commit).
2. **Check.** The batch's `check:` lines run on the merged tip, in the row's
   tree, with greens reused by input hash (`wsp check --affected --run`'s
   rule), so a batch whose merge was a fast-forward of one branch reruns
   nothing.
3. **Clean and green:** the batch is closed, its trees go by `check_tree`'s
   rule (`leg_tree::discard`), and the next batch may open.
4. **A conflict or a red step** does not stop the row for a person. The run
   adds one piece, `merge batch 2` or `fix batch 2: <step>`, in a new batch
   right after this one (the only write that inserts a batch, and it is
   allowed because nothing after it has opened). Its overview names the
   branches still to merge and the conflicting files, or the red step and the
   piece whose line it is; its done-when is the batch's checks green on the
   merged tip. It stands in the row's tree, as a batch of one. This is legs'
   join in the shape pieces already have.

**The last batch closed, the row goes to review**, by the run: the account is
the pieces table with each piece's caption, and the review runs the row's own
check (`wsp check <row>`), which now includes every done piece's `check:`
lines, labelled `<n>.<label>`. Then the verifier opens as for any row.

## The run

The levels-not-edges rule `cycle.rs` follows, with legs as the template.
Nothing new remembers anything.

- **Opening.** `cycle::step` reads `## Pieces` on every `doing` member, and on
  a `todo` member whose group is running (a planned row starts with its first
  batch, needing no seat). It opens the lowest open batch's pending pieces
  with no seat, in record order, under the row's `cap:` and through the room's
  `Gate::admits`, each opening written under the store lock before the spawn.
  While a seat holds the row's claim, no piece opens (`take_legs`' rule).
- **Places.** A running piece is one working agent in `room::working`, counted
  off its entry's seat as a leg is. **Who goes first**: verifiers and barrier
  checks, then pieces and legs of rows already started, then new members.
- **One predicate.** Every "doing with no seat" repair that steps aside for a
  staged row (`wedged`, `sent_back_unseated`, the room's `holds_nothing`)
  steps aside for a row with pieces too. `legs::staged` has 21 call sites (11
  in `cycle.rs`, the rest in `ending`, `repair`, `untaken`, `closing`,
  `cmd_task`, `room`, `leg_seat`); each is read and moved to
  `inner::run_inside(t)`, true for either section, or kept where it means
  legs alone.
- **Outside a list.** `wsp piece done` opens what is ready itself, detached,
  through the gate, as `leg done` does, and the repair tick catches a lost
  launch or a piece held at the cap.
- **Ending.** The tick ends a done piece's seat once, from `/`, as it ends a
  done leg's.

## The verdict

**One verifier, for the row** (Ed, 2026-11-08, to save time). Its order gains
the pieces block: for each piece, its title, done-when and the `wsp check`
result of its lines on the row's tip, and the instruction to judge the
done-when **item by item, per piece**, as an unrun sub-task's is judged today
(`docs/legs.md`, *Staging a row already running*). A piece that needs a verdict
of its own is a sub-task instead (*Choosing*).

**A blocking verdict names pieces.** The run adds one rework piece per piece
named (`rework of 3`), in a new batch after the last, on the named piece's
tier and `why:`, with the verdict as its overview and the named piece's
done-when as its own. A verdict that names no piece gets one rework piece over
the row. The row goes back to `doing` and the run opens the batch. This is
legs' rework, one per named piece instead of one per row, because pieces are
separable and the others stand.

**The risk this takes.** The verifier reads more in one session. On
react-pass-3, two of 35 members were sent back (compounding-189 four times,
compounding-196 twice): six rework seats, $3.68, 13% of the list. With one verifier, a fault in
piece 3 is found after the last batch rather than minutes after piece 3, and
its rework runs at the end. The mitigations are already in the design: each
piece's checks run at `piece done` and again at its batch's close, so the
mechanical half of the verdict is settled before the verifier opens. The proof
row measures the block rate against the per-row baseline.

## Transience

- **At land**, the `## Pieces` section is folded into the row's log: one line
  per piece, `piece 2 · board component · done · <caption> · ran
  sonnet-5-5/medium 23 turns · checks green · done-when: <first 200 chars>`.
  The entries go. `wsp show <piece-id>` on a landed row prints that line and
  says the piece was folded.
- **`wsp piece promote <id>`** turns a pending or blocked piece into a real
  sub-task: `wsp add --parent <row>` with the piece's title, its overview and
  done-when as the overview, and its `check:` lines as `## Checks`. The entry
  becomes `promoted: <new id>` and no longer holds its batch. The verb prints
  the `wsp worklist add` line that would run it; it does not edit a list. This
  is how an undone piece outlives its row, and how a piece that turned out to
  need its own verdict gets one.
  Spelled `wsp piece promote <row> <n>`. The new row is filed under the
  piece's row in its project, at `todo`, with the piece's overview, then
  `Done when:` and its done-when, as its `## Overview`, and the `check:`
  lines as its `## Checks`. A piece on another tier prints its `--agent`
  with the line. The row's log says `piece 2 promoted to <id>`, and the
  batch closes without it. A seated piece promotes only a piece it added.
- **`wsp piece drop <id> "why"`**: the work is not needed. It no longer holds
  its batch, and the fold says why.
- **`wsp piece reset <id> "why"`** (`wsp piece reset <row> <n> "why"`): a
  `blocked` piece, or a `running` one whose seat is gone, goes back to
  `pending`; its seat, tree and start go from the entry, its binding is
  cleared and its start misses forgiven, and the row's log says
  `piece 2 reset by hand: <why>`. A seat still there is refused until
  `--force`, which ends it first, as `wsp leg reset` does. A pending, done,
  dropped or promoted piece is not reset, and a seated piece may not reset
  another.

## Seen and told

- `wsp show <piece-id>` prints the parent's header line and the entry whole;
  `wsp show <row>` renders the pieces table, folded, as it renders legs, and
  `--pieces` prints them whole. `--json` gives `pieces: [{n, id, title,
  batch, state, seat, tree, caption}]`.
- `wsp tell <piece-id>` reaches the piece's seat (from the entry). `wsp note
  <piece-id>` writes to the row's log prefixed `piece 2:`. `wsp say` from a
  piece's pane says it as `wsp-501-2`.
- `wsp wip` shows each running piece as its own line, `wsp-501-2 board
  component`, under a row line that counts them (`pieces 2 running, 5 done of
  9 · batch 2 of 4`). `wip --json` gives the seat `role: {is: piece, task,
  piece}`. compound reads the label and `wip --json`, so it sees them with no
  change.
- The caption tail is the newest piece caption, as for legs.

## The barrier

**A row with pieces is one member.** The barrier waits on its verdict, reads
its landed commit and files, and runs the group's checks as for any member.
Its pieces are inside the row, already merged and judged. The group's `xN`
counts members, so the row is one; its pieces take places on the machine (the
room) and under the row's own `cap:`. `wsp worklist show` draws the pieces
under the member, folded. The governor hears about a piece only when it
blocks.

Why not each piece as a member: that is the cost pieces exist to remove. A
barrier that counted pieces would want each one's landing and files, which
is a sub-task's bookkeeping under another name.

## Choosing: legs, pieces or sub-tasks

This is the rule the size check (`wsp-499`) uses, and the line for the brief.

- **Legs** when the parts share one tree, one built artefact or one line of
  reasoning and are judged together: a build, then drives of that build; a
  scout, a plan, the builds it sets out. Parts with no done-when of their own.
- **Pieces** when the work divides into parts that each have their own
  done-when and check lines, can be merged in batches, and do not each need a
  verdict: fifteen components in fifteen directories; a record, a seat and a
  run built against one design.
- **Sub-tasks** when a part needs its own verdict, review or life in the
  backlog: it can land without the others, someone will look for it later, or
  it belongs to another repo (a piece lives in its row's tree, so the
  cross-repo prerequisite of `wsp-495` is a row, by `wsp-495`'s route).

**The size check** (`wsp-499`, part 2) fires on the signals that row lists. A
row passes with `## Legs`, `## Pieces`, sub-tasks, or `--whole --why`. Its
message names the signal and points by shape: signal (b), a build over five
minutes followed by drives, is legs; signal (a), three or more separate
observations, is pieces when the observations are of separate parts and legs
when they are of one built thing; signal (c) is either. The check cannot tell
those apart, so the planner chooses, and the check only asks that one is
chosen.

**The brief line** (a draft for Ed to place in `~/wsp/agents.md`, replacing the
"Decomposing while planning" draft on `wsp-499`):

> - Decomposing while **planning**? Coupled parts judged together (one tree,
>   one built artefact: a build, then drives of it) are **legs**: `wsp leg add
>   <id> <name> --order FILE`, ending in one last leg. Separable parts, each
>   with its own done-when and checks, judged by the row's one verifier, are
>   **pieces**: `wsp piece add <id> --from FILE`, in batches. Parts that need
>   their own verdict or a life in the backlog are **sub-tasks**: `wsp add
>   "…" --parent <id>`, made members with `wsp worklist add <list> <id>
>   --sub`. A done-when with three or more separate observations, or a build
>   of more than about five minutes before a drive, means one of these; if one
>   seat really is right, say why with `--whole --why`.
> - Working a **piece** and it is bigger than it looked, or it needs a change
>   elsewhere in this row's repo? Commit, add pieces in a later batch (`wsp
>   piece add <row> … --batch next`), then `wsp piece done` and stop.

## Departures from the proposal

- **Proposal 6, "a piece may itself run as legs": not in this version.** A
  piece that is coupled work is split into pieces in consecutive batches (a
  batch of one is a chain: build in batch 2, drives in batch 3), or promoted
  to a row that runs as legs. Legs inside pieces would give the run a second
  level: entries inside entries, a binding of row, piece and leg, trees named
  `<row>-<n>.<leg>`, and two join rules. Consecutive batches express the same
  work with none of it. If the trial finds a piece that needed legs, it is a
  row of its own then. A row still has legs or pieces, not both.
- **A piece's block blocks the piece** (proposal silent; legs block the row).
- **The run types the row's review** after the last batch, rather than a
  last piece. Legs need one last leg because the merge needs a seat that
  reads the batons; pieces close mechanically and only a conflict or a red
  step costs a seat.
- **`cap:` per row**, which legs declined, because the proposal asks for
  batches "under a cap" and a pieces row is wide by design.

## Cost

**Per piece, against the same work as a row** (the 0–40 turn bands, where
pieces will mostly sit):

- **Saved:** the verifier seat, a median $0.13–0.16 and 80–97 seconds of a
  place; the store row and its backlog entry; list membership; a packet; the
  barrier's per-member reading.
- **Added:** a share of one larger verifier. The 150+ band's median is $0.31
  over 539 seconds; allow up to twice that for breadth, about $0.02–0.04 a
  piece in a row of fifteen, so the net saving is about $0.10–0.14 a piece. The batch close is seatless and costs the
  check's seconds; a conflict or a red step costs one fix seat.
- **Unchanged:** the piece's own seat (start, reading, work), its tree, and
  the planner's writing.

**On one real larger work: react-pass-3 group 2**, fifteen component rows
(compounding-168 to 182), each its own directory with a done-when and check
lines: the shape pieces are for. Measured: workers $4.64, seventeen verifier
seats (180 and 182 each had a second) $2.49, governor $0.71, barrier $0.17,
total $8.01. The workers' turns summed to 246.

| run as | verifiers | group total | saved |
|---|---|---|---|
| fifteen rows (measured) | 17 seats, $2.49 | $8.01 | |
| one row of fifteen pieces (estimate) | 1 seat, $0.31–0.62 | $5.83–6.14 | $1.87–2.18 (23–27%) |

Across react-pass-3, groups 1 to 3 (32 rows, $5.98 of verifiers) would have
been three rows with pieces: $0.93–1.86 of verifiers, saving $4.1–5.1 of
$28.67 (14–18%). The store would have held 7 rows of the pass's 35.

**Time is not saved.** In group 2 the verifiers took 28 seat-minutes against
the workers' 43, sharing the machine's four places. One verifier of about nine
minutes, on the critical path at the end, gives back what the freed places
gain. The estimate counts no wall-time saving, and the proof row measures it.

**What would move it.** A larger verifier than estimated; a block rate higher
than per-row verifiers had (a late rework batch costs a seat and the end of
the critical path); or merge conflicts between pieces of a batch, which today
are rows' landings at the barrier instead and would move, not appear.

## The trial

`wsp-510`, 2026-11-08, on master 507e5b0. Two runs: a sandbox run of the
mechanics, then one real larger work (`wsp-495`) run as pieces on the list
`pieces-trial`, priced with `wsp attempts <row> --cost` and `wsp worklist
cost`.

### The sandbox run

`wsp sandbox --fake`, with a stub `compound-sup` standing in for the real one
(the run spawns on compound by default, and a fake herdr does not catch that:
without the stub every seat would have been a real agent). Each stub seat
reported `SessionStart` and a turn as Claude's hooks do. The list `proof` had one
member, `demo-001`, on a scratch repo: pieces 1 and 2 in batch 1, piece 3 in
batch 2, each with one `check:` line.

With no person's verb, the run:

- opened batch 1, with a tree for each piece cut from the row's branch;
- closed it: merged both branches into the row's tree and ran the two checks
  on the merged tip;
- opened piece 3 in the row's tree;
- closed batch 2 and typed the row's review, with the pieces table as the
  account;
- opened one verifier, whose order carried the pieces block (each piece's id,
  done-when and checks on the row's tip);
- landed on the hold, folded the three pieces into the row's log, ran the
  barrier and closed the list.

Only the seats were played: three `piece done`, one `verified --holds` and
the barrier's `go`, each from its own seat. Piece 1's start to the land took
about 3 minutes.

Found along the way:

- **Piece trees outlive their batch** (`wsp-532`). `piece done` pokes the run
  while the piece's agent is still in its turn. So the batch closes with each
  piece's seat standing in its tree, and the trees are kept. Nothing discards
  them once the tick ends those seats, and `leg_tree::sweep` reads only
  `<row>.<leg>` names. The trees and branches were still there after the list
  closed.
- **A sandbox's list outputs reach the real `~/.compound/artefacts`**
  (`wsp-533`): `wsp sandbox` leaves `WSP_ARTEFACTS` unset.
- `wsp attempts --cost` gives a start that never seated a clock that runs to
  the next start (8m and 6m for about a minute each). The fold says `ran ?`
  for a seat with no session.

### The real run

`wsp-495`, three gaps as three pieces, was the one member of `pieces-trial`.
It could not join `pieces` group 6, because a running group takes no joins,
and every later group would have waited on this row's barrier. Batch 1 was
piece 1 (the run, in `src/cycle.rs` and `src/cmd_worklist.rs`, at sonnet
high with a why) beside piece 2 (the seat, in `src/room.rs` and
`src/waiting.rs`). Batch 2 was piece 3 (`docs/prerequisites.md`, written
against what batch 1 built). The list waited 1h30m behind the thermal
throttle before it started. That wait is the machine's, so it is not counted
below. Times are UTC, 2026-11-08.

| step | seat | clock | cost |
|---|---|---|---|
| piece 1, batch 1 | sonnet/high, 29 turns, peak 132k | 11m | $0.80 |
| piece 2, batch 1 | sonnet/medium, 30 turns, peak 80k | 5m | $0.41 |
| batch 1 close: merge, 3 checks on the merged tip | none | 22:28 to 22:31:49 | none |
| piece 3, batch 2 | sonnet/medium, 7 turns | 51s | $0.12 |
| landing check at d484d1e: sent back, `build` exit 137 | none | 165s | none |
| piece 4, batch 3: rework, no change | sonnet/medium, 4 turns | 2m | $0.07 |
| **verifier**, one seat, holds | sonnet/high, 7 turns | **185s** (spawn to verdict) | **$0.15** |
| governor | 3 turns | 37m standing | $0.11 |
| barrier: full suite 714s, then its reading seat | sonnet/high, 3 turns | 12m | $0.07 |
| **list total** | 7 seats, 83 turns | 1h | **$1.73** |

`wsp attempts wsp-495 --cost` prices the same seats at $1.25, against $1.40
by `wsp worklist cost`. The difference is how each counts cache writes, and
both are floors. The table uses the list's figures.

- **The verifier against *Cost*.** It cost $0.15, inside the per-row median
  ($0.13–0.16). Its clock was 185 s, about twice the per-row 80–97 s, on a
  machine at load 11 and throttled to 60–64% of its clock, which slowed
  every seat that hour. A row of three pieces did not need the breadth
  allowance (the 150+ band's $0.31). As three rows, the same work would have
  had three verifiers: $0.39–0.48 and three places for 80–97 s each. The
  saving is $0.24–0.33 on verifiers. Less the fix piece's $0.07, the net is
  $0.17–0.26, which is $0.06–0.09 a piece against the estimate's
  $0.10–0.14. That is low, and both causes are below.
- **Blocks.** The verifier did not block. No piece blocked. The landing
  check sent the row back once, and no piece was at fault: when piece 3's
  seat ended in the row's tree, the finished-row sweep stopped the check's
  own `cargo build`. It spares a `wsp` but stops anything named `cargo`
  before it asks what started it (`wsp-541`). Piece 4 found nothing to fix.
  The run then opened a verifier on the unchanged commit without running
  `build` again, so the land rested on the verifier and the barrier's full
  suite (`wsp-543`). The send-back cost 6 minutes of wall time and $0.07.
- **Wall time.** 26 minutes from piece 1 started to the land (22:16:03 to
  22:42:10), against 19 minutes of piece seats summed and a 14-minute
  critical path through them (11m, then 51s, then 2m). The rest is the
  batch closes' checks (about 3½ minutes for batch 1, where `cargo test
  prerequisite` took 95 s on the merged tip), the killed landing check, and
  the verifier. The barrier's full suite (12 minutes) was the longest single
  clock in the list, and pieces do not change it.
- **Merges.** Batch 1's two branches merged clean. Their files did not
  overlap, so a merge had nothing to collide on. No fix piece was for a
  conflict.
- **Legs.** No piece wanted legs or added a piece. The one coupling, the
  doc written against the code, was batch 2, as *Departures* says.
- **`cap: 3`** never bound. Batch 1 had two pieces. What held the row was
  the room and the throttle, before it started.

### The decision

- **The cost estimate holds, with a narrower range at the small end.** For
  a row of a few pieces, the one verifier costs a per-row verifier, not the
  150+ band, so a piece saves a little under a whole verifier's $0.13–0.16.
  Here it was $0.06–0.09 net, because the run lost a send-back to a sweep
  defect. Without `wsp-541` it would have been $0.08–0.11. The estimate for
  a wide row (react-pass-3 group 2, fifteen pieces, 23–27%) is unchanged:
  this trial had no row that wide, so it neither confirms nor moves that
  figure.
- **Time is not saved**, as *Cost* said. Wall time was the critical path
  plus the seatless closes and one verifier, and the barrier's suite was
  larger than any of them.
- **`cap: 3` stays.** It was not reached, and the machine's room bound
  first. Nothing here argues for a higher default on this hardware.
- **A piece's block blocks the piece: kept, untested.** No piece blocked. A
  send-back at the landing check is the row's, and the run answered it as
  it should: with a rework piece in a new batch, not by reseating the row.
- **No legs inside pieces: confirmed** for this run. No piece wanted them.
- **What the trial moved instead is the land gate.** A pieces row's review
  is typed while its last piece's seat may still stand in the tree. A
  sweep that fires when that seat ends must not reach the check that has
  just started there (`wsp-541`). A rework piece that changes nothing must
  not let the run's review skip the step that failed (`wsp-543`). Both are
  filed. Until they land, a pieces row can be sent back for nothing, and
  can then land without that step green.

## Not this

- **Pieces as sub-tasks with a flag.** A sub-task is a store row, an id that
  outlives the work, and a member if it is to run. The saving is in not
  having those.
- **Pieces as legs with a done-when.** Legs are one graph ending in one leg
  that reads batons and types review. Pieces are levels with their own checks,
  closed by the run. Folding them into one record would give every leg a
  done-when it does not want, or every piece a last leg it does not need.
- **A verifier per piece**, or a verifier per batch. That is the per-row cost
  under a new name. Ed decided one.
- **Each piece a barrier member.** See *The barrier*.
- **A store change for state-core** (`wsp-483`). Pieces are a section on the
  row, as `## Legs` and `## Verification` are, and move with them.

## Build rows

Sub-tasks of `wsp-501`, on the list `pieces` as groups 2 to 6, following the
React batching order: the contract and record, then isolated parts, then
wiring, then the verbs, then the live proof. Each row's checks are `cargo
test <module>::` lines with one-word labels.

| group | rows | cap |
|---|---|---|
| 2: contract | **pieces-record** `wsp-502` (`src/pieces.rs`: the section, its rules, `wsp piece` add, show, drop and cap, `wsp show` rendering) · **pieces-ids** `wsp-503` (the grammar, the slug refusal, the store audit) | x2 |
| 3: parts | **pieces-seat** `wsp-504` (`spawn --piece`, the binding, the brief, `piece done` with its checks, start and end lines, review and block from a piece) · **pieces-close** `wsp-505` (`src/piece_close.rs`: a batch's merge and check on scratch repos, the fix piece it adds) | x2 |
| 4: wiring | **pieces-run** `wsp-506`, on Opus (`cycle::step` opens batches under both caps, closes them, types review, `inner::run_inside`, the launch from `piece done`, repair) · **pieces-verdict** `wsp-507` (the verifier's pieces block, rework pieces from a blocking verdict, the fold at land) | x2 |
| 5: verbs | **pieces-address** `wsp-508` (`show`, `tell`, `note`, `say`, `wip` on piece ids; strays and stale) · **pieces-promote** `wsp-509` (`promote`, a seated piece adding to later batches, the docs) · **wsp-499** (planner legs and the size check, which now names pieces) | x3 |
| 6: proof | **pieces-proof** `wsp-510`, on Opus (a sandboxed run to verified with no hand step, then one real larger work run as pieces, measured against this table) | x1 |

Each group's stop is on the list; each row's done-when and checks are on the
row, and its `after` names the rows it needs. The two Opus rows say why in
their logs: wsp-506 reorders the run's opening under the lock, and wsp-510
records a decision from measurements.
