# Legs: a row run as a graph of agents

`wsp-287`, 2026-10-17, revised for parallel legs by `wsp-296`, 2026-10-18.
Nothing here is built yet. The build rows are at the end.

## The decision

A row can run as a set of **legs**. Each leg is a seat of its own, with its
own kind, model and effort and a fresh context. It gets a small **baton** from
each leg it waits on, ends at a commit, and is recorded as one attempt on the
row. A leg has no ticket. The legs are a `## Legs` section on the row, written
the way `## Verification` is (`wsp-188`): the entry is the record, and the
only one.

**The legs form a graph, not only a chain** (Ed, 2026-10-17: "make sure that
those substages are also able to run in parallel where possible, similarly to
a worklist"). A leg waits only on the legs it names, the way a member waits
only on the members its `wsp after` names. Legs that wait on nothing still
running start together, under the machine's agent cap. A chain is the graph
where each leg waits on the one above it, and it is still what a record with
no `after:` lines means.

This answers the row's first question (roles inside a row, or more rows with
`after`) the way Ed did on 2026-10-17: roles inside the row, with no ticket per
stage. It is not the overseer dispatching subagents inside one session. Why not
is under *Not this* below.

What the revision changed from the chain design (29779a8, decision d1 on
`wsp-287`), so a reader of the first version can find it:

- Each entry can carry `after:`, and the record must end in exactly one leg
  (*The record*).
- **A leg holds no claim.** It is recorded on its entry, the way a verifier
  is recorded on its pass, because a row has one claim and two legs cannot
  both hold it (*A leg is a seat*).
- **A leg that can run beside another gets a tree of its own**, branched from
  the row's branch. Its branch is merged back before the leg that waits on it
  opens (*Trees*).
- A leg that waits on several legs reads all their batons, under a total cap
  (*The baton*).
- A rework leg waits on the last leg and becomes the new last leg (*The run*).
- **A leg runs on Sonnet unless it says otherwise, and says why** (Ed, through
  compound's custodian, 2026-10-18: "we should also primarily use sonnet for
  such sub-rows"). The member's `agent` line no longer sets a leg's tier.
  Any other tier is written on the leg with a reason, such as Opus for a
  design leg that decides (*The record*).

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
- **Opus doing the volume.** A leg that reads or edits to a plan does not need
  Opus. So **every leg runs on Sonnet unless the leg says why not**. A leg that
  decides may be worth Opus, and it can decide from a baton and a few targeted
  reads.

**What the tiers cost.** wsp's price table (`cmd_burn::PRICES`, read
2026-10-06) prices every Opus at $5 input with cache reads at a tenth, which
is $0.50 a million read. It prices Sonnet 5 at $2, which is $0.20. The table's
own comment says Opus 5.5, which is what the fleet runs (`ran opus-5-5/high`
on the release lines), lists at $4/$20 with cache reads at 0.05×. That is
**$0.20 a million, the same as Sonnet's**, and the table does not model it.
So the figures below are given both ways. Every dollar the flight comparison
and `wsp burn` printed for an Opus 5.5 row used the table's rate, which is 2.5×
too high for cache reads. legs-measure fixes the rate (*Build rows*).

The measured inputs: g ≈ 1.7k tokens a turn (compound-476 went from 43k to 481k
over 254 calls, and 512 reached 491k). R ≈ 60–80k, which is what the flight
builders read before their first edit. Break-even growth is therefore
2R ≈ 120–160k, or **about 70–95 turns**. That fits the first guess of ~80.
Break-even does not depend on the tier: it compares a row with the same row
split, both on the same model.

A row shaped like 476 (T = 254, S₀ = 43k, k = 4, R = 70k), counting cache reads
only:

| run as | cache reads | at the table's rates | at Opus 5.5's listed rate |
|---|---|---|---|
| one Opus session (as 476 ran) | 66M | $33 | $13 |
| one Sonnet session | 66M | $13 | $13 |
| four legs, plan on Opus, three on Sonnet (d1) | 38M | $10 | $8 |
| **four Sonnet legs** | 38M | **$8** | **$8** |

Four Sonnet legs also pay four starts, ~43k written to the cache each, which
is about $0.43 in all. So **~$8 against ~$33** at the prices wsp prints
today. At Opus 5.5's listed rate the baseline is ~$13. There, the tier saves
nothing on cache reads, and the split's own 28M fewer tokens (~$5.6, 42%)
is the whole saving. Either way, the saving that does not depend on a
price-table entry is the split's. These figures are estimates. The trial row
exists to replace them with measurements, and Sonnet may take more turns than
Opus for the same work, which would raise g and T.

Below break-even, legs cost money. A leg's start writes ~43k to the cache
($0.11 on Sonnet, $0.22–0.27 on Opus), and it pays its R again.

Running legs side by side changes none of that arithmetic. It changes the
time a row takes. *When to stage* says when that is worth having.

## Names

The list is called `substages`, but `stage` is taken: `src/stage.rs` is the
caption word (`wsp-242`). `handoff` is also taken: it is an agent moving its
claim to another task (`cmd_agent::hand_off`). So:

- **leg**: one agent's part of a row. `## Legs` and `wsp leg …`.
- **baton**: what a leg leaves for the legs that wait on it.
- **the last leg**: the one leg nothing waits on. It ends the row.
- **siblings**: two legs where neither waits on the other, by any path. They
  are the legs that may run at the same time.

## The record

```text
## Legs
- scout · claude sonnet medium · done · pane cpd-31 · ended
  order: Read the three call sites of … and say which layer owns …
  baton: three layers: store (src/legs.rs new), spawn (cmd_spawn.rs:2274–2330), …
- plan · claude opus high · done · pane cpd-34 · ended
  why: decides which layer owns the leg record, from the scout's three options
- build-store · claude sonnet medium · running · pane cpd-36 · tree wsp-287.build-store
  after: plan
- build-spawn · claude sonnet medium · running · pane cpd-37 · tree wsp-287.build-spawn
  after: plan
- finish · claude sonnet medium · pending
  after: build-store, build-spawn
```

- One entry per leg. Each entry has a name, a policy (a `model::Policy`
  line), a state (`pending | running | done | skipped`), and the pane and
  ending clauses that `verification::Pass` already has. A leg in a tree of
  its own adds a `tree` clause (*Trees*).
- **`after:`** is indented under the entry, with the order and the baton.
  It lists the legs this one waits on, by name. An entry with no `after:`
  waits on the entry above it, and the first entry waits on nothing, so a
  chain needs no `after:` lines at all and every d1 record still means what
  it said. To make a leg wait on nothing, write `after: -`.
- A leg is **ready** when every leg **upstream** of it, by any path and not
  only the ones it names, is `done` or `skipped`. A skipped leg satisfies the
  wait. The legs that waited on it are told it was skipped, and why, in their
  brief. The wider rule is what a skip needs: with A → B → J, A running in the
  row's tree and B skipped, J names only B, and J's join merges into a tree
  nobody may be standing in. Readiness waits on A, so J opens when A is done.
  The skip itself is accepted while A runs, because a governor skipping a
  pending leg while the one before it works is ordinary. Refusing it would
  leave the same J ready the moment A finished anyway.
- The order and the baton are each capped at 3,000 chars. Above the cap the
  text is refused, never truncated. A baton's cap can be lower when several
  batons meet at one leg (*The baton*).
- **No cost here.** A leg's spend goes on its end line in the row's log (see
  *Measured*). A second copy would be two records of one fact, which is how
  wsp-148 ended up with five rows.
- **The tier is Sonnet unless the leg says otherwise.** A leg with no policy
  runs on `claude sonnet medium`, one constant in `legs.rs`, and `wsp show`
  prints it so the record never hides a tier. The member's `agent <member>:`
  line and the group's line do not reach a leg. They say how the row runs
  unstaged, and staging it is the choice to run it on cheaper seats.
- **Any other policy needs a `why:`**, indented under the entry and capped at
  300 chars: `wsp leg add … --agent "claude opus high" --why "decides …"`.
  It applies to another model, another effort or another kind alike. The
  reason has to be about the leg's work, such as Opus for a design leg that
  chooses between approaches. "The row is hard" is a reason to stage it, not
  a reason to give a leg Opus. A policy other than the default with no
  `why:` is refused, and so is a `why:` on a default leg, which would be a
  reason for nothing.
- Opening a leg (`running`) is written under the store lock **before** its seat
  is spawned, so it is the idempotence key, as a running pass is. As with
  verification bookkeeping, it does not move `updated`.

**What every write is checked against.** Each `wsp leg` write builds the
record it would leave and refuses it, naming why, if:

- an `after:` names a leg that does not exist, or names the leg itself;
- the edges make a cycle. The refusal names the cycle, as `wsp after` does
  (`after::refuse`, refusal 3): `plan → build-store → plan`;
- more than one leg has nothing waiting on it. The record must end in **one
  last leg**, because that is where the row's work comes back into one tree
  (*Trees*), and where `review` is typed;
- it holds more than **8 legs**, counting rework. A row that needs more should
  be split into rows.

The one-last-leg rule is why legs are added with both ends:
`wsp leg add <task> <name> [--agent "<policy>"] --order FILE
[--after <leg>,…] [--before <leg>]`. `--after` is the new leg's `after:`
(default: the current last leg). `--before X` adds the new leg to X's
`after:`. With no `--before`, the new leg becomes the last leg, as in the
chain. A plan leg setting out two parallel builds types:

```text
wsp leg add wsp-287 build-store --order store.md --after plan --before finish
wsp leg add wsp-287 build-spawn --order spawn.md --after plan --before finish
```

**`add` appends.** A new entry goes at the end of the section, never in the
middle, because an entry with no `after:` waits on the entry above it and an
insert would change what that entry waits on. Every other leg keeps its waits
exactly. A new leg gets an `after:` line only when it differs from the entry
above, so a chain stays a chain with no `after:` lines. `--before X` with no
`--after` gives the new leg the legs X waits on now and adds the new leg to X,
since "the current last leg" would be X itself and a cycle.

Every record in between those two commands ends in one leg (`finish`), so no
write sees two ends. `wsp leg after <task> <name> <leg>…` replaces a pending
leg's `after:`, the way `wsp after` replaces a task's.

**Who may change it:**

- A person or the row's governor may add, edit, re-point or skip any
  `pending` leg at any time: `wsp leg skip <task> <name> "why"`.
- **A seated leg may change only pending legs downstream of itself**: legs
  that wait on it by some path, and new legs whose every `after:` is itself
  or downstream of it. This is the chain rule ("only legs after itself") read
  on a graph. It is how a plan leg sets out the build legs, and how a build
  leg that hits a decision calls in an overseer: it adds a `decide` leg on
  Opus whose order is the question, with `--after` itself, and a leg to
  carry on after that. A leg cannot touch a sibling's downstream legs that
  are not also its own. Two siblings that both re-point the last leg are
  each changing a leg downstream of themselves, which is allowed, and each
  write is checked whole under the lock, so the second one sees the first
  one's result.
  A seated leg's `--before X` needs X downstream of it too. A re-point is
  judged by the waits it adds: a sibling the leg already waited on may stay.
- Nobody edits a `running` or `done` leg, and `--before` names a pending leg.
  A new leg may wait on a done one. It is ready at once.

## A leg is a seat

`wsp spawn <task> --agent --leg <name>`, which the run normally types for you.
The seat handling that already exists applies, with one change from d1: **a
leg holds no claim.**

The chain design had each leg claim the row. A row has one claim
(`claims.json` is keyed by task), and `claim` refuses while another live pane
is bound to the task (`cmd_agent::live_holders`). Two siblings could not both
hold it, and making one of them the holder would make the other a second-class
seat. A verifier already solves the same problem. It is an agent on a member
that claims nothing and is recorded on the member's open pass, and its brief
finds it by its seat (`cmd_spawn.rs:2269`, `:2459`). A leg is recorded the same
way, on its entry, whether it runs alone or beside a sibling. One rule for both.

- **Record.** Opening a leg writes its seat onto the entry before the agent
  starts. The brief, `wsp say` and `wsp leg done` find the leg from the seat.
  The pane is bound to the row (`task_in_hand` reads the binding first), so
  every command that asks "which task is this pane on" gets the row.
- **Attempt.** The row's log gets a start line and an end line for each leg:
  `leg build-store started by pane cpd-36 · spawned at sonnet/medium` and
  `leg build-store ended after 41m · ran sonnet-5-5/medium · 63 turns ·
  4.1M cache 120k in 31k out · peak 152k`. These have the same clauses as
  the claim and release lines and are written by the same code, so
  `cmd_attempts` reads each leg as one attempt with tier, turns and spend.
  Pairing is by leg name, not "the last open attempt"
  (`attempts_of` pairs that way today), because two siblings' lines
  interleave.
- **Tier.** The tier is the leg's policy, or Sonnet when it has none,
  checked with `Kind::tier` before anything is opened, as a member's is. The
  start line says which: `spawned at sonnet/medium`, or `spawned at
  opus/high · why decides …` for a leg with a reason, so `cmd_attempts` can
  set each Opus leg beside the reason it was given.
- **Fresh context.** Every leg gets a new seat and a new session. It is not a
  `/clear` in the same pane: a clear cannot change the kind or the model, and
  only some kinds have a measured `clear_command`.
- **cwd.** The row's worktree, or the leg's own tree when it has siblings
  (*Trees*).
- **Label.** `wsp-287 · build-store`, in the same form as `t-1 · verifying`.
  d1's `3/5` is dropped: in a graph the position in the list does not say how
  far along the row is.

**Ending a leg:** `wsp leg done <task> --from FILE --caption "…"`. The command:

1. **Refuses if the leg's tree has uncommitted tracked changes.** Every leg
   boundary is a commit. That lets the legs that wait on it trust `git log`
   and `git diff` over the baton, and it gives the run the "reset at each
   commit" lever the flight comparison pointed at.
2. Refuses a baton over its cap or a missing caption. The caption is the
   tail `stage.rs` shows after the word.
3. Writes the baton, marks the leg `done`, and writes the end line with the
   leg's spend.
4. Pokes the run.

**`wsp review` from a leg ends the row** when no other leg is running. The
remaining pending legs are written `skipped · ended by review`. A scout that
finds nothing to do should not have to walk the graph. With a sibling still
running, `review` is refused and names it: the row is not finished while work
on it is open. The last leg always types `review`, since nothing else is
running by then. **`wsp block` from a leg** blocks the row, as it does today,
and siblings already running finish their legs. Nothing new starts on a
blocked row. A leg that finds the baton too thin to work from blocks rather
than re-deriving it. That keeps each leg as the check on the legs before it.

## Trees

**A leg that runs alone stands in the row's tree. A leg that has a sibling
gets a tree of its own.** One tree with disjoint files was the other choice,
and it fails four ways that wsp has already met:

- **The commit boundary.** `wsp leg done` refuses a tree with uncommitted
  changes. In a shared tree, one leg's half-made edit refuses its sibling's
  ending, and a `git add` sweeps in the sibling's work. Three agents in
  `~/claude/wsp` did exactly that on 2026-08-15, and it is why `overlap.rs`
  exists.
- **The build.** A green build has to mean *this* leg's HEAD is green. That
  is why every checkout got its own build tree (`27f1a18`; `sharing.rs`).
  Two cargo builds in one tree have also crashed the linker here. Either way,
  a failed build is reported as if it were a compile error in your change.
- **Disjoint is only a hope.** Files are observed, not declared. A plan can
  give each leg its own files. It cannot stop a leg needing `main.rs` to wire
  in what it made.
- **The tests.** A test run in a shared tree sees a sibling's half-made
  edits.

So each sibling works in `<trunk>/.worktrees/<task>.<leg>` on branch
`<task>.<leg>`, made when the leg opens from the row's branch at its tip.
A leg opens only once everything it waits on is done and merged (below), so
that tip already holds every commit it waits on. Task ids carry no `.`, so the
name cannot be read as a task's tree, and it does not parse as
`<list>-check-<N>` (`check_tree::named`).

Which legs have siblings is read off the graph, not off what happens to be
running. Two siblings get trees of their own even if the cap makes them run
one after the other. Otherwise the tree a leg stands in would depend on
timing, and the merge would too.

**The merge back is at the leg that waits.** A leg that waits on legs in
trees of their own is a **join**. Before a join opens, its spawn merges each
of those branches into the row's branch, in the row's tree, in `after:` order,
with `git merge --no-edit`. Then the join opens: in the row's tree if it has
no siblings (the last leg never has), or in a new tree off the merged tip if
it does. No agent is standing in the row's tree when this happens. Every leg
running at that moment is a sibling of the join, since it neither waits on the
join nor is waited on by it, and a leg with a sibling has a tree of its own.

- **Clean.** The join's agent starts on the merged work, and its brief lists
  the merge commits.
- **A conflict.** The spawn aborts that merge (`git merge --abort`), so the
  row's tree is clean at a commit, and stops merging. The join's brief names
  the branches still to merge and the files that conflicted. Merging them in
  the tree it opened in is the join's first job. **Two legs touching one file
  is therefore not an error.** It is a merge. If the edits are in different
  places, git merges it and nobody is told. If they overlap, the leg that
  reads both batons merges them. A seat that can decide is better at that
  than a refusal is.
- **Warned early.** `wsp leg done` from a sibling lists the paths it changed
  that a sibling's branch has also changed, and the join's brief carries that
  line. A plan leg that wrote disjoint orders hears about it before the merge
  does.

**The tree goes when its branch is merged.** By the same rule as a check's
tree (`check_tree::discard` and `tidy`, `wsp-289`): the leg is done, its
branch is in the row's branch, no live pane stands in it, and
`git status --ignored` shows nothing beyond `target/`. Anything else keeps it,
and `cycle.log` gives the path. The branch goes with the tree. `stale` and
`strays` in `cmd_checkout` read `<task>.<leg>` as the row's, so a leg tree is
never reported as an orphan, and a closed row's leg trees are cleaned up with
the row's own tree.

A sibling's tree starts cold: an empty `target/`. That costs time and disk,
not tokens (23s against 3s warm for this crate, measured 2026-08-18 in
`sharing.rs`; far more on compound, whose trees reach 7–10 GB). *When to
stage* counts it.

## The baton, and what a leg reads

The flight plans ran to 16–21k chars, and the builders read the tree anyway.
So a baton does not try to replace reading. It aims the reading. The
suggested shape (the cap is enforced, the shape is not):

- **Done**: the commits, and what they changed, one line each.
- **Where**: `path:line–line` for what the next leg needs to read. Pointers,
  not contents.
- **Decided**: what the next leg must not reopen.
- **Left**: anything this leg could not settle.

**The caps.** One baton is at most 3,000 chars, as before. The batons coming
into one leg are at most **6,000 chars together**. So a leg's baton is capped
at the smaller of 3,000 and 6,000 / n, where n is the number of legs in the
`after:` of a leg that waits on it (the largest such n, if several legs wait
on it). One leg waiting gives 3,000, as in the chain. A join of three gives
2,000 each, and a join of four gives 1,500. The cap is worked out when
`wsp leg done` writes the baton, from the record at that moment, and the
refusal says which join set it. A baton of pointers fits in 1,500 chars,
which is about twenty `path:line` lines. A join wider than four is a sign the
plan split too finely. The total is 6,000, not 3 × 3,000, so that a join
brief stays under the size of the flight's plans, which were read and then
ignored.

A leg's brief is today's brief plus a bounded block:

- `leg build-store on wsp-287 · after plan · 2 running, 2 done of 5`
- the leg table, one line per leg (name, tier, state, `after:`, caption)
- its own order
- **the baton of every leg in its `after:`, in full**, in `after:` order. A
  skipped one appears as `skipped: <why>`.
- every other done leg's baton as its caption only, with
  `wsp leg show <task> <name>` to read one whole
- for a join: the merges the spawn made, the branches it could not merge and
  their conflicting files, and the overlap lines from its siblings' endings

The order (3,000) plus incoming batons (6,000) plus the table and the rest
keeps the block to about 10k chars at its widest. A chain leg's block is the
same ~8k as before.

A plan leg's split of the work goes into the **orders** of the legs it adds,
each capped at 3,000 chars. No single document carries the whole plan, and
each builder gets only its own share. A plan that fans out should also say,
in each order, which files that leg owns. The merge is how a mistake there
comes to light. Nothing stops the leg from editing other files.

## The run

The same levels-not-edges rule `cycle.rs` follows. Nothing new remembers
anything.

- **Starting.** `cycle::step` reads `## Legs` on every `doing` member. It
  opens every leg that is pending and ready, has no seat yet, and is on a
  row that is not blocked, in record order, each one through the room's
  `Gate::admits`. That means under the store lock, with the count taken in
  the same section as the opening write (`room.rs`, `wsp-231`). A staged
  member's legs that wait on nothing are how it starts.
- **The cap.** The machine's agent cap is `crate::room`, which is `wsp-231`'s
  four on this seat. It is the cap the chain design already started legs
  under. **Each running leg counts as one working agent**, like a member or a
  verifier. `room::working` gains a block that counts legs by the seat on
  their entry, as it counts passes. A leg that does not fit stays pending and
  is said once in `cycle.log`, as a held member is. **A group's `xN` still
  counts members**: a staged member is one member however many legs it is
  running, because `xN` limits how much of a group is in flight and the
  machine cap limits the machine. There is no per-row cap on how many legs
  run at once. The graph says which legs can wait on each other, the
  machine says how many run, and the rest wait. That is how a worklist group
  works under the seat cap.
- **Who goes first** when places are short: a verifier and a barrier check,
  as `wsp-231` decided, then a ready leg of a row already started, then a new
  member. A leg is unfinished work on a row the run has already begun, and
  `wsp-231` put finishing ahead of fanning out.
- **Ending.** The tick ends a `done` leg's seat once, the way `end_passes`
  ends a verifier, and records the outcome on the leg's entry. Ending a
  sibling's seat runs from `/`, as `cycle::despawn_seat` does for a check, so
  the tree is not kept because the ending process is standing in it.
- **Outside a list.** A row someone staged and spawned by hand has no run to
  step it. `wsp leg done` launches the ready legs itself, detached, as `poke`
  does, through the same gate: a launch is the run, not a person asking, so it
  waits for a place. The daemon tick's repair catches a lost launch, and a
  leg held at the cap, in both cases.
- **Must not double-start.** The existing "doing with no seat" repairs
  (`wedged`, `sent_back_unseated`) must leave a staged row to the leg path.
  If they did not, they would spawn a whole-row worker beside the legs. A row
  with no claim is now the ordinary state for a staged row, so the exclusion
  is "has a `## Legs` section", not "has a running leg".
- **Verifier.** Unchanged. It runs once, on the row, after the last leg's
  review and land, and checks the row against its overview. The roles are
  the means and get no verification of their own. Its order gains one line:
  the row ran in legs, here is the table, and a verdict that blocks names the
  leg whose work fails.
- **Send-back.** A blocking verdict appends one `rework` leg. **It waits on
  the last leg, and becomes the new last leg**, so nothing waits on it and
  the one-last-leg rule holds. It runs in the row's tree, which by then holds
  every leg's merged work. It runs on Sonnet, like any leg, unless the leg
  the verdict names had another tier. Then it takes that tier and that leg's
  `why:`, prefixed `rework of <leg>:`. If the verdict names several legs, it
  takes the dearest of their tiers. A verdict that names no leg gives no
  reason for anything but the default, so that rework runs on Sonnet. Its
  order is the verdict. It may add legs downstream of itself like any seated leg, so a
  rework that splits into parallel fixes can. The member goes back to
  `doing` and the step starts it. A row that has hit the 8-leg cap goes to
  its governor.
- **Barrier.** Unchanged. It reads `wsp show`, which renders the leg table.

## Measured

The row asked how `wsp burn` can report per row and per role. Every number
needed is already written somewhere except three, and those three are the
whole addition:

- **The leg's start and end lines** (*A leg is a seat*), which carry the
  claim and release lines' clauses under the leg's name.
- **The spend clause on a verification pass**, written when its seat ends,
  using the same `Ran` the end line uses. The flight comparison counted the
  verifier's cost, and today only `wsp burn`'s per-seat tally sees it.
- **A `peak` clause on `Ran`**: the largest context any turn of the attempt
  read. Peak was the flight's fourth column, and it is the number staging is
  meant to bring down.

`wsp attempts <row> --cost` (or `wsp burn --row`; the build row picks the name)
prints one table per row: each attempt grouped by leg, then each verifier
pass, with tier, turns, peak and dollars, and a total. With parallel legs it
adds two numbers: the row's **wall time** (first leg start to last leg end)
beside the **sum of its legs' times**, and the join legs' turns marked as
such. The first pair is what overlap bought. The second shows what merging
cost. `--list <list>` does this for every row of a worklist. With that, the
flight comparison is one command instead of an afternoon of transcript
arithmetic. It also works for rows that are not staged, so the baseline can
start building up before legs exist.

Attempts are paired by leg name. Today's `attempts_of` pairs a release with
the last open claim, and two siblings' lines would be crossed by it.

*Built (wsp-290):* the name is `wsp attempts <row> --cost [--list <worklist>]
[--json]`. The `peak` clause follows `turns` on a release or leg end line; a
verification pass carries `ran`, `turns`, `peak` and `spent` clauses written when
its seat is ended. Dollars are priced at report time from the recorded tokens,
at the dearest tier a moved session named, and a cache write counts at the input
rate (the record keeps `in` as input plus writes), so a total is a floor. The
price table now carries the cache-read multiplier per row: Opus 5.5 and Sonnet
5.5 at 0.05x, Fable 5.1 and Mythos 5.1 at 0.025x. Haiku 5.5's prompt-length
tiers are not modelled.

## Seen

- **wip and census.** A leg holds no claim, so `wip` reads running legs off
  `## Legs`, as it would read a running pass. Each running leg is a line,
  `wsp-287 build-store`. The row's own line says `legs 2 running, 2 done of
  5`. `wip --json` gives each such seat `leg: {name, after, index, of}`
  (`index` is record order, kept for sorting). The census is the backend's
  list of panes (`place::census`), so a leg's pane is in it already, under
  its label.
  A leg's seat is `role: {is: leg, task, leg}` and its `task` is the label,
  `wsp-287 · build-store`. `index` counts from 1. Its `kind`, `model` and
  `effort` are the entry's policy, and `held` is since the leg opened: there is
  no claim to read either from. A leg that is `done` and whose seat the tick has
  not ended yet has `needs_you: false`, since it is finished and nobody is owed
  anything from it. `wsp ls --json` gives a staged row the same `legs` objects
  `wsp show --json` has, and an unstaged row none.
- **Caption.** `stage.rs` keeps its word (`doing`). The tail is the newest
  leg caption written, so a row reads `doing · scout: three layers, plan
  next`. With siblings it is whichever finished last. No new word, and
  nothing derived that a person did not write.
- **compound.** compound reads the seat label and `wip --json`, so every leg's
  seat reaches it with nothing changed in compound. A row with two seats is
  new to it, though. Whether its row draws them both, and drawing the leg
  graph in the row's detail, is compound's own row.
- **`wsp show`.** Renders `## Legs` like `## Verification`: one line a leg,
  with `after:` shown when it is not the line above, and order and baton
  folded to their first line. `--legs` prints them whole.

## When to stage

This is the rule for the handbook, kept to what can be checked when the list
is written:

- **Stage a row** when it is expected to run past ~80 turns, or when it
  crosses more than one layer (each layer becomes a build leg).
  Expected-past-80 in practice means it reads more than it edits (476 made
  ~5 edits in 254 calls), touches more than one module family, or has a
  plan that names more than one step under *Order*. The threshold is in
  turns of the row as one session, whatever its tier. It is where a split
  starts saving against the same row on the same model (*Why*).
- **The legs are Sonnet.** Every leg of the default graph runs on Sonnet.
  A leg gets another tier only for its own work, written as its `why:`. The
  one leg the default graph expects might have one is a plan that has to
  **choose**: between designs, between layers to own a record, or anything
  the overview leaves open. Opus there, with that choice as the reason. A
  plan that only divides work the overview has already designed stays on
  Sonnet. Against a row that would have run on Opus, a Sonnet leg also gains
  the tier difference (see the table in *Why*). That is a bonus. It is not
  why a row is staged, and it is the whole difference only at the table's
  Opus rate.
- **The default graph:** `scout` (reads, and writes a baton of pointers),
  then `plan` (decides from the baton plus targeted reads, and adds the
  build legs; Opus only with a `why:`), then `build-<layer>` × N (edits,
  runs the loop, commits), then `finish` (the full test, rebase, land,
  review). The plan leg decides whether the builds are a chain or siblings.
- **Do not stage** a row under ~80 turns that sits in one layer. The extra
  starts and re-reads cost more than they save, against the same row
  unstaged on the same tier. Which tier an unstaged row runs on is the list's
  choice, as it is today.

**When the builds run side by side.** Overlap does not change the token
saving. Each leg's spend is the same whether its sibling runs at the same time
or after it, so T·(1 − 1/k)·(gT/2 − R) holds for k legs however many of them
overlap. What overlap changes:

- **Time.** If a row's T turns are T_s run in series and T_p split across p
  siblings, its wall time goes from about (T_s + T_p)·τ to (T_s + T_p/p)·τ,
  where τ is the time a turn takes, plus one cold build per sibling.
- **Extra cost.** Each sibling pays a cold build (time and disk, not tokens).
  The join pays the merge. That is nothing when it is clean, and some turns
  when it conflicts. The siblings also cannot use each other's reading: two
  siblings each pay R where a chain's second leg might have read less after
  the first.
- **Time is only bought when places are free.** On a list already at the
  machine cap, a row's second sibling takes a place another member would
  have used, and the list finishes no sooner. Siblings pay when the row is
  its group's **long pole**: the member the barrier waits on, or a group of
  fewer members than the cap.

So: **make the builds siblings** when they edit disjoint files (the plan can
name each one's files), each is long enough that its share of the time beats
a cold build (on compound, several minutes a tree), and the row is the long
pole of its group. Otherwise keep them a chain. A chain costs the same tokens
and has nothing to merge.

The threshold is a calculation from four Opus rows. Sonnet's g and T on the
same work are not measured yet, and the overlap rule is reasoning with no row
behind it at all. The trial row is where all three get measured and either
confirmed or moved.

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
  barrier slot per stage. Ed asked for no ticket per stage. Legs borrow
  `after`'s *shape* (a wait on named things, cycles refused when the edge is
  set) without its rows.
- **A verifier per leg.** The next leg reads the baton and blocks if it is
  thin, and the row's verifier reads the outcome. A per-leg verifier would
  pay a session start to check something that is checked twice already.
- **One tree for siblings with disjoint files.** See *Trees*. It would save a
  cold build per sibling, and it would break the commit boundary, the build,
  and the tests.
- **A per-row parallel cap.** The machine cap and the graph are enough. A row
  wider than the cap runs as places come free, like a group with no `xN`.
  If a row ever needs one, it is an `xN` on `## Legs`, with the same meaning
  as a group's.

## Build rows

Each row is filed `-p wsp`, has a **Done when**, and uses `wsp after` as shown.
The six from the chain design were filed early and are parked (wsp-290 to
wsp-295). **All six carry over, each with its overview rewritten to this
version. None goes. One is new**, legs-tree, because the trees are a piece of
their own and can be built beside legs-seat. The barrier rewrites the six and
files the seventh.

1. **legs-measure** (wsp-290, *changes*): a row's cost is one command. As
   before: the spend clause on verification passes, `peak` on `Ran`, the
   per-row and per-list cost table. Changed: `cmd_attempts` reads the leg
   start and end lines as attempts, **pairs them by leg name** rather than
   with the last open claim, and the table adds the row's wall time beside
   the sum of its legs' times, with join legs marked. Each leg not on Sonnet
   is shown with its `why:`. New: `cmd_burn::PRICES` gets an Opus 5.5 row at
   its listed rate ($4/$20, cache reads at 0.05×), which its own comment
   names and the table does not model. Otherwise every Opus 5.5 baseline the
   trial compares against costs 2.5× its real cache-read price, and the
   comparison would credit legs with money the tier never cost. That row
   re-reads the price page first, as the table's comment asks. *Done when*
   `wsp attempts compound-512 --cost` (or the chosen name) prints worker,
   rework and verifier spend with tier, turns and peak; a captured log of two
   siblings' interleaved lines pairs each end with its own start; and an
   `opus-5-5` attempt is priced at the Opus 5.5 rate. Tested on captured log
   lines. *After:* nothing. It measures unstaged rows too.
2. **legs-record** (wsp-291, *changes*): `## Legs` on a row. As before: the
   parse and write module (`legs.rs`, modelled on `verification.rs`),
   `wsp leg add | skip | show`, the caps (8 legs, 3,000-char order and
   baton), and `wsp show` rendering. Changed: `after:` with its default
   (the entry above) and `after: -`; `--after` and `--before` on `add`;
   `wsp leg after`; the refusals (unknown leg, itself, a cycle named as
   `after::refuse` names one, more than one last leg); the downstream
   authority rule; readiness (done or skipped); the baton cap of
   min(3,000, 6,000 / n); and the tier: no policy reads as the Sonnet
   constant, `--why` and the `why:` line, and both refusals (another tier
   with no `why:`, and a `why:` on a default leg). *Done when* every rule in
   *The record* and the baton cap has a test that builds exactly the record
   it refuses or accepts, including a d1 record with no `after:` reading as a
   chain, and a leg with no policy on a member whose `agent` line says Opus
   reading as Sonnet.
   *After:* nothing.
3. **legs-seat** (wsp-292, *changes*): a leg is a seat. As before: `spawn
   --leg`, the leg brief block, `wsp leg done` (clean tree, caps, caption,
   poke). Changed: **a leg claims nothing**. It is recorded on its entry as a
   verifier is on its pass, and the pane is bound to the row. It writes the
   leg start and end lines in place of claim and release. It spawns at the
   leg's tier, which is Sonnet when the entry names none, and the start line
   carries the `why` of any other tier. The brief carries
   every incoming baton and the join block. `review` is refused while a
   sibling is running, and from the last running leg it skips the rest.
   `block` blocks the row and lets running siblings finish. *Done when* a
   sandboxed (`wsp sandbox --fake`) staged row goes scout → done → (spawn of
   the next leg by hand) → review, each leg's start and end lines are on the
   row, no claim is written, and `review` from one of two running siblings
   is refused. *After:* 2.
4. **legs-tree** (*new*): a sibling's tree and the merge back. The
   `<task>.<leg>` tree and branch made at open for a leg with siblings,
   decided from the graph; the join's spawn merging its `after:` branches in
   order, aborting at a conflict and handing the rest to the brief; the
   overlap line on a sibling's `leg done`; discarding a merged leg's tree by
   `check_tree`'s rule, from `despawn` and `tidy`; and `stale` and `strays`
   reading `<task>.<leg>` as the row's. *Done when*, on scratch repos as in
   `check_tree::`: two siblings each get a tree off the row's tip; a clean
   join merges both and the trees go; a conflicting pair leaves the row's
   tree clean at a commit with the branch and files named; a tree with an
   untracked file is kept and named; and a leg tree is never reported as a
   stray. *After:* 2.
5. **legs-run** (wsp-293, *changes*): the run steps the graph. Changed from
   the chain: `cycle::step` opens **every ready leg** through `Gate::admits`;
   `room::working` counts legs by their entry's seat; the order verifier and
   check, then legs, then new members; the tick ending done legs' seats from
   `/`; the detached launch from `leg done` going through the gate; the
   `wedged` and `sent_back_unseated` exclusion by `## Legs`; the verifier's
   order line; and a blocking verdict appending one `rework` leg after the
   last leg, on the tier rule in *The run* (Sonnet, unless the named leg had
   a `why:`, which it inherits). *Done when* a `wsp sandbox
   --fake --stage` list with one staged member whose plan leg fans out into
   two siblings runs every leg to verified with no hand step; with the seat
   capped at 2 and a second member ready, the siblings take places ahead of
   the new member and the third agent waits; a blocking verdict produces
   exactly one rework leg, after the last; and a second advance starts
   nothing twice. *After:* 3, 4.
6. **legs-seen** (wsp-294, *changes*): wip, captions, label, handbook.
   Changed: `wip` reads running legs off `## Legs`, not claims; the label is
   `<task> · <leg>` with no `n/m`; `wip --json` carries `leg: {name, after,
   index, of}`; the row line counts running and done legs; and the handbook
   rule is *When to stage* as revised: legs on Sonnet, another tier only
   with a `why:` about that leg's own work, and *When the builds run side by
   side*. *Done when* `wsp wip`
   and `wsp wip --json` show both running siblings of a sandboxed staged row,
   and the handbook carries the rule. *After:* 3.
7. **legs-trial** (wsp-295, *changes*): the comparison, measured. As before:
   run two or three large rows (expected past 80 turns) staged on the default
   graph, and compare them with 1's table against unstaged Opus rows of
   similar scope. Changed: the staged rows run on Sonnet legs, with Opus
   only on a leg whose `why:` gives a reason. At least one is staged all
   Sonnet, so the split's saving and the tier's can be told apart. At least
   one fans its builds out as siblings. The Opus baselines are priced at
   Opus 5.5's rate from 1. The decision also covers the overlap rule, using
   the wall time against the sum of leg times and the join's merge turns,
   and Sonnet's turns per row against Opus's on similar scope, which would
   move g and T. *Done when* a decision that confirms or moves the ~80-turn
   threshold, the Sonnet default, and the when-to-fan-out rule is recorded
   with the table behind it. *After:* 1, 5,
   6.

1 and 2 can share a group. 3 and 4 can share the next, since both rest only
on 2. 5 and 6 come after them, and 7 comes last.
