# Governor kind pass (design, wsp-560)

Build: wsp-585. Open choices are blocked on wsp-560 for Ed; this doc settles neither.

Design: a governor kind pass. Written to Ed's d1 (explore a governor pass over the worklist, at start or at plan time, that sets each group's kind). Nothing here is built by this row; the build is filed as its own row and the two open choices are raised with `wsp block`.

## What a kind does, and what a wrong one costs
`scope::decide` returns Full for every doubt. A group with no kind, a contract group, an insitu group and the list's last group all run the full suite. Only `components` and `wiring` can narrow, and only if the landed files hold no global path, the project's safety net (`full-every`, default 5) is not due, and some landed file was observed. So the kinds fall into two classes:
- Setting `contract` or `insitu`, or leaving no kind, can never make a barrier weaker than today. A wrong kind in that direction costs wall time only.
- Setting `components` or `wiring` is the one step that can narrow a barrier. A wrong one there risks a missed regression, bounded by the file checks (global paths widen to Full, shared paths widen to dependents) and by the safety-net full run every Nth scoped barrier and the last group's full run.
The pass therefore has one rule: it may only choose a narrowing kind when it has positive evidence for it, and every other case it leaves as "no kind" or sets a full-running kind. It must never guess narrow.

## What the pass reads
For each group not yet started, whose kind is empty (a kind the planner or Ed already wrote is never overwritten; the pass only fills blanks):
1. The members' declared files: each row's `--ref` paths, its done-when checks and, once rows have landed, the files they landed (`worklist::touched`). At list start nothing in a later group has landed, so the pass reads declared paths and the overview's named files; at barrier time the observed landed files are the truth and `decide` already checks them.
2. The project's `## Barrier` section: `shared` and `global` globs, and the project's unit layout (the per-unit test commands that make a narrow run possible at all). A project with no unit-scoped test commands gets no narrowing kinds: there is nothing for `components` to narrow to.
3. The four kinds:
   - `contract`: any member path matches `shared` or `global`, or the title/overview says types, schema, protocol, or contract.
   - `insitu`: a member's checks include a drive or in-situ step.
   - `components`: every member path falls inside one unit's directory (or several units, none shared), and none match `shared`/`global`.
   - `wiring`: every member path is under screens/wiring paths the project names and the rest are units.
   - otherwise: leave it empty.

## Mixed groups
fleet-fixes is the case in the Evidence: wsp-only fixes, compound-sup changes and installs inside one group, so no one kind is true. The pass does not invent a blended kind. A group whose members straddle classes (a unit and a shared/global path, or two incompatible project layouts) gets the most conservative kind that fits, `contract`, or stays empty. Both run Full today, so a mixed group loses nothing against the status quo. The gain for such a plan comes from the pass reporting which members forced the full run, so the planner can split the group at plan time: one group of unit-only rows, which narrows, and one of the rest.
The honest limit: a plan like fleet-fixes may still get few narrow groups. The pass does not claim the 101m back by itself; it makes the cause legible and narrows the groups where the evidence is clear.

## When it runs: two placements, one function
The kind decision is one pure function over (group members' declared files, project barrier config) that returns a kind and a one-line reason, or none. Two places could call it:
- At list start, in `wsp worklist go`, next to the nudge in 04f777b: the governor fills blank kinds on groups not yet started, logs each decision with its reason, and the list shows them. Advantage: runs on every list including hand-written ones and sees the final membership. Disadvantage: late, since rows may be too coarse to narrow.
- At plan time, in the planner's seat when it writes groups: sets kinds as it groups rows, and can reshape the groups to make narrow ones. Advantage: can fix the mixed-group problem by splitting. Disadvantage: depends on the planner doing it, which is the failure this row records (the plan wrote none).
Both can coexist: the plan-time pass proposes and the start-time pass fills what is blank. Which of the two is the primary is Ed's choice; it is raised, not settled.

## How it relates to the nudge already built (04f777b)
The nudge names groups with no kind at `wsp worklist go` and prints the `wsp worklist group <id> N --kind …` hint. It sets nothing. Under the design it becomes the fallback report: after the pass has run, the nudge names only the groups the pass could not decide, with the reason. Whether the nudge stays advisory or becomes an enforced step (go refuses a list with unkinded non-last groups until each has a kind or an explicit `--kind none`) is the second open choice.

## Safety and rollback
- The pass writes a kind through the same path as `wsp worklist group --kind`, and logs `group N kind set to X by the governor: reason` so the verdict and the report can tell a governor kind from a planner kind.
- `wsp worklist group <id> N --kind <k>` overrides it; a governor kind is never written over a human one.
- The barrier report already records scoped vs full and `scope::Note`; it gains the origin of the kind so the retro can score the pass: groups narrowed, seconds saved, and any narrowed barrier whose failure a later full run caught (the safety-net hits).

## Tests the build owes
Pure-function tests per kind and per mixed case; "never overwrites a set kind"; "never chooses a narrowing kind with no unit layout or with a shared/global path"; a go test that a blank-kind list gets its groups filled and logged.

## Open choices for Ed (raised with wsp block, unsettled)
1. Governor at list start versus planner at plan time (or both, the plan-time pass proposing).
2. Nudge versus enforced step.
