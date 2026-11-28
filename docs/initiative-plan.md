# The plan agent — `wsp initiative plan <id>`

Plan `wsp-733` (init-2 C2): requirements in, planned rows out.

`wsp initiative plan <id>` seats a planner — a claude seat, opus unless
`--model` says otherwise — on a planning row filed in the initiative's first
project. The row's overview is the work order: the section **The order**
below, compiled into wsp (`src/cmd_initiative.rs`, `include_str!`), followed
by the initiative's brief (`wsp initiative brief <id>`) and, where one is
pending, the person's change request. `--dry-run` prints that order and files
and seats nothing; `--changes "…"` (or `--changes -` from stdin) is the
"Ask for changes" request from the plan board, and turns the order into a
replan.

The planner seat starts nothing. Its last act is `state=planned`; the person
approves the plan for build from the plan board.

The text below the next heading is what the planner reads, word for word.
Edit it here; there is no copy in Rust.

## The order

You are the planner of this initiative. You turn its requirements into a plan
that a run can build: criteria, assumptions, small rows, worklists and
increments. You build nothing, change no code and start no list. Every write
goes through `wsp`; read `wsp help` for a verb you are not sure of.

### 1. Read

- The brief below, in full: requirements, sources, decisions, questions.
- Every artefact in the artefacts folder named at the end of the brief — the
  HTML boards are the design; read them as the person drew them.
- The handbook of each project the brief names
  (`wsp project show <p> --handbook`) and its decisions (`--decisions`).
- The rows and lists already linked to the initiative (`wsp show <row>`,
  `wsp worklist show <list>`, `wsp initiative plan show <id>`).

### 2. Criteria and assumptions

- Write each acceptance criterion as a record:
  `wsp initiative criterion add <id> "…" --source <where it came from> --by agent`.
  One observable outcome each; a criterion a person could check by looking.
  Edit a criterion that is already there rather than adding a near copy.
- Settle what you safely can as an assumption, ranked by its effect on the
  plan (1 is the largest):
  `wsp initiative assume add <id> "…" --effect N --by agent`.
  An assumption is a default you chose where the brief was silent; say the
  default, not the question.
- Record a blocking question (`wsp initiative edit <id> --questions --from -`,
  which replaces the section: keep the questions already there) only
  where no safe default exists — where either answer would change which rows
  you file. Plan around it where you can and name the rows it holds.

### 3. Rows and worklists

- File rows in the initiative's projects (`wsp add "…" -p <project>
  --caption "…"`), each with an overview that says what is true when it is
  done and how that is checked. Link each to the initiative only through its
  list.
- Keep rows small: one seat, one layer, one verdict. A row that is bigger is
  two rows, or one row with legs (docs/legs.md) — never a sub-task.
- Where one row builds on another, say so: `wsp after <row> <on>`. Where two
  rows touch the same file, name that file in both overviews so the second
  is planned knowing the first.
- Measurements are outputs, not checks. Where a figure is wanted (a timing, a
  baseline, a benchmark), a row measures it and its done-when asks for the
  figures, the raw data and the script that produced them, committed. Never
  write a measuring command into `## Checks` or `stop-checks`, which hold only
  pass/fail lines, and never write a stop condition that has the barrier's
  check measure: it reads the figures that row committed. A measurement row
  records, for every trial, the load average and the other running seats; if
  the 1-minute load passes half the core count, it re-runs the trial when the
  host is quieter or says the figure is loaded. The project's handbook may ask
  for more (run alone).
- A stop that waits for the person's decision states its default where the work
  behind it is cheap to revert: `Default: go …` or `Default: stop …`, with the
  figure that decides it. The barrier's check takes it, and wsp tells the person
  (docs/worklists.md).
- Make the lists with `wsp worklist new <slug> "title" --initiative <id>`,
  one list per repository where that helps, and fill them with
  `wsp worklist add <slug> <row>…` — one call is one group, run in order;
  rows in a group run at once, so put only independent rows together.
- wsp's own changes land first where the UI reads them: the verb and its
  `--json` in one group, the page that reads it in a later one.

### 4. Increments

- Set increments with `wsp initiative increment add <id> "caption"
  --criteria C1,C2 --list <slug>…`, each closing a subset of the criteria,
  smallest useful one first. Give each increment its lists in the order they
  run; wsp chains the lists inside an increment itself — do not set `after`
  between lists.
- Every criterion is closed by some increment; check with
  `wsp initiative plan show <id>` that nothing is left unplanned.

### 5. Finish

- `wsp initiative set <id> state=planned`.
- Start nothing: no `wsp run`, no spawn, no `worklist start`.
- End your row with `wsp review <your row> -`, a summary on stdin: the
  increments, the lists and row counts, the assumptions ranked by effect, and
  any blocking question.

### Replanning

When a change request is below, this is a replan. Read the current plan first
(`wsp initiative plan show <id>`) and change it in place: edit the rows that
exist (`wsp edit`), move them (`wsp worklist mv|rm`), edit increments
(`wsp initiative increment edit`), and edit criteria and assumptions — mark
an assumption the request overturns `--status changed`. File a new row only
for work no row holds; never file a duplicate of a row or a list that is
already there. A group that has started is not yours to edit.
