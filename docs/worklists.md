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
