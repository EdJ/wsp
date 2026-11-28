# Planning a worklist: one seam, one group

## The rule (wsp-068)

Rows on the same seam (a function, a file region or a code path) never share a
group. Put them in consecutive groups, or make them one row with legs. Members
run side by side only when the planner names disjoint files for each, and every
row's overview names the seam and the files it touches. The project handbook
states the same rule.

Two members of one group that edit one file both start from the same trunk.
The first to land is fine. The second is sent back on a rebase conflict, and
its seat does the work again on the new trunk.

## The check (wsp-859)

`wsp worklist add <list> <row>… --group N`, `wsp worklist add <list> <a> <b>…`
(a new group of several rows) and `wsp worklist mv <list> <row> --group N` each
compare every row joining the group with every member already in it, and with
each other. A pair that names a file in common gets:

- a warning on stderr, naming the group, both rows and the file, with the
  `wsp worklist mv <list> <row> --after N` that puts the second row in the
  group after;
- a line in the list's log, `same file in group N: <a> ~ <b> both name <file>`,
  where the barrier and the governor read it.

A pair that was in the group already is not reported again.

**What it reads.** It reads each row's own account of the work: the Overview,
Evidence, Fix, Done when, Checks and Details sections, plus the title. It never
reads the Log or the notes. The reader is `crate::duplicate::names`, the one a
follow-up's "covers <file>" check uses (wsp-565). A file is any token with a
path-like stem and a source extension (`rs`, `md`, `toml`, `json`, `txt` and
the others `is_file` lists). The files a row names are the ones it *expects* to
touch, so a row that names none is not checked. The comparison is
`crate::duplicate::shared_files`.

**A warning, not a refusal.** A file named is not proof of an edit. Two rows
can name one file and edit far-apart parts of it. The edit goes through, and
the planner decides. The same two rows in consecutive groups are accepted
without a word, since the barrier between the groups puts the first row on the
trunk before the second starts.

The check runs only when rows are put into a group. Rows that a running
group's barrier attaches (`followup`) are checked by wsp-565's duplicate check
instead.

## Merge and hand-off rows: decisions first (wsp-866)

A row that brings other history in asks everything that is the person's before
it resolves anything. trunk-green's merge row asked as it went, and 44 of the
list's 116 minutes were waiting on the person (Jesper, 2026-11-23).

**Which rows.** A title that starts with "Merge" and names `origin/`, upstream,
or `into … master|main|trunk`; or the tag `merge` or `hand-off`
(`crate::merge_row::is`).

**At planning.**

- The row runs as two legs. `decide` trials the merge, lists every conflict and
  every person-shaped choice (merge point, design clash, guard change, push),
  and asks them in one `wsp ask <row> --person --wait`. `finish` merges as the
  answers say. Both say `packet: off`. `decide` leaves the `push` line in its
  baton, and `finish` is told to read it from `wsp worklist show <list>`.
- The list's overview records the push go on a line of its own, `Push: go (who,
  when)` or `Push: no`. `wsp worklist show <list>` prints it as `push`, and a
  row that finds it there asks nothing about the push.

`wsp worklist add` and `wsp worklist status <list> planned` warn on a merge row
with no legs, printing the two `wsp leg add` commands, and on a list with no
`Push:` line. They refuse nothing.

**A merge row left as one seat** gets the same rule in its packet.

**Landing.** Merge, never rebase. A branch that holds the merge and contains
`master` lands by fast-forward, so nobody is asked for a `git merge --ff-only`.

**Who pushes.** Not the member: its seat ends at submit, and the verifier lands
the row after it. The land does not read the `Push:` line.

- A shared row: wsp pushes, as part of the land. The branch goes to the trunk of
  the project's `remote:` (`origin` where it names none), fast-forward only,
  before the local trunk moves.
- A private row: the land fast-forwards the local trunk, and no step of the run
  pushes it. A `Push: go` on the list is not acted on by wsp: the trunk stays
  local until someone pushes it by hand.

## A red the trunk has

A row owns a red on the trunk by saying so in its log: `wsp note <row> "trunk red: <test>"`,
the test or `file:line` as the runner prints it. While that row is open and not landed:

- the first check in its project that names the red pulls the row into the current group of the
  running list that holds the checked row. The barrier waits on it, and it starts beside the
  group's `xN` (the seat cap still holds). It runs on the group's agent line, or on the one it had
  in a later group of that list. A row another list holds stays there, and the checked row's log
  says so.
- a row's check (`wsp check`, the review rule steps, the rerun at a land, piece checks) passes over a step whose only
  failures are named ones. It still fails on any other failing test, a build error, a crash, a run
  cut short, output it cannot count, or a line chaining several commands. While a red is known,
  cargo steps run with `--no-fail-fast` (after `env`, `NAME=value` and `+toolchain` too). A cargo
  run that stopped at a failed target all the same, as one inside a script can, is not held over.
- compound-react's quiet `check.mjs` report is read too: a `✗ <file>:<line> › <title>: …` line
  is named by its file and line or by its title up to the first `: `, never by its message. It is
  held over only where `playwright` is the one red stage and every failure it counts is such a
  line, since an earlier red stage stops it before Playwright.
- a named red in a serial-mode spec file is not held over: Playwright skips the tests after a
  failure there, and the quiet report lists no skipped test. The `*.spec.*` or `*.test.*` file the
  failure names is read in the checked tree, under the step's `cd` directory, for
  `describe.configure({ mode: "serial" })` or `describe.serial`. A file that cannot be found
  there fails the step too. The red's row is still pulled in.
- the red's own row, the barrier and the end build do not hold over it.

## A done-when that waits on another list's run (wsp-870)

An item such as "the next check of <row> ends with no exit 143" is shown by
another list's run, and this list cannot cause it. It is a dependency, not an
ask and not a watch (Jesper, 2026-11-24):

1. The member runs `wsp after <row> <what produces the check>`, naming a row
   another list holds, or the list itself where the row has already landed and
   the check is a later run of that list.
2. It commits and runs `wsp release <row>`. The row stays at `doing`. It holds
   no place of its group's `xN`, and the run neither reseats it nor names it
   silent. The group's barrier still waits for it.
3. Once the row has landed, or the list is done or cancelled, the run seats the
   member again. It reads the check and submits the item as held, or not held.

`wsp after <row> --clear` calls the wait off. A list that holds the member, or
that waits on one that does (`wsp worklist after`), is refused. A row run as
legs or pieces is not held this way (wsp-835).

