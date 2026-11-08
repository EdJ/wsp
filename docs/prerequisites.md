# A prerequisite outside a row's files

## What happened

On `react-pass-3`, group 2 stood still overnight on one member. `compounding-182` needed a change in compound's host-core, outside its own files. The brief offers a worker legs (work left on its row) and block (a decision), so it blocked, asked the person, and held its seat in a tool call for hours. The primary seat filed the change as `compound-1005` and set `after`, but `wsp worklist add` was refused ("it is the group being run"), so nothing could schedule the prerequisite without a person.

## The route a member takes

A change in another repo, or another crate's owner, is a prerequisite and not a decision.

1. File it: `wsp add "…" -p <project> --caption "…"`, with an overview and a done-when. Do not put it on a list; the run does that.
2. Name it: `wsp after <your row> <new row>`.
3. Commit what you have.
4. `wsp release <your row>` and stop. Do not wait in a tool call.

Block only if the change itself needs a person's choice.

## What the run does

Each tick, before it starts anything (`cycle::pull_prerequisites`), the run looks at the members of the group it is running. A row such a member names with `after` is added to that group just ahead of the member when no list holds it and it is not done. This takes no person, and it is logged on the list, on both rows and in `cycle.log`: `wsp: prerequisite <row> added to <list> group <n>`.

The run does not pull in a row that another list holds (the member's row says "wsp: not adding" once), a row that is done, or a row named by something that is not a member. An `after` wait already holds a todo member until the row lands, and the barrier already waits on the member, so it passes with both.

A member that has released its seat (`doing`, no claim, no pane, the prerequisite line newer than its last start) is held and does not count against the group's cap, so an `x1` group starts the prerequisite. Once every row it named has landed, the run seats it again on its existing row ("again: its prerequisite landed"). That reseat does not count against the cap of reseats.

`wsp worklist add` on the running group is still refused for a person, and the refusal names this exception.

Not covered: a member that is at `blocked` and did not release is not reseated on landing.

## What a seat waiting on a person costs

A seat that asks the person (`wsp ask … --person --wait`) keeps its pane. Once the open ask is older than 10 minutes the seat holds no place under the cap (`room::HELD_ASK_AFTER`), so a question that waits all night does not keep a place from other work. The ask stays open, `wip` still shows the seat waiting on the person, and nothing nudges or ends it. Answering it makes the seat count again. This only limits the cost; releasing after the ask, as above, is still better when the work cannot go on.

## The brief line

For Ed to place in `~/wsp/agents.md`:

> Need a change outside your row's files (another repo, another crate's owner)? It is a prerequisite, not a decision: `wsp add "…" -p <project> --caption "…"` with an overview and done-when, then `wsp after <your row> <new row>`, commit what you have, `wsp release <your row>` and stop. Block only if the change itself needs a person's choice.
