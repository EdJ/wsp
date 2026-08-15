# wsp

Workspace and task control plane for [herdr](https://herdr.dev).

herdr knows the *live* facts — which pane exists, where it's rooted, whether its
agent is idle or working. It deliberately knows nothing about *work*. `wsp` owns
the durable half (hierarchical projects, inherited tags, tasks, notes), joins the
two, and pushes the join back into herdr's sidebar as metadata tokens.

Design rationale and the full spec: `../wsp-spec.md`.

## Install

```sh
cargo build --release
install -m 755 target/release/wsp ~/.local/bin/wsp
wsp init                       # creates ~/wsp (git) and ~/.local/state/wsp
herdr plugin link "$PWD/herdr-plugin"
```

The plugin's `[[startup]]` launches `wsp daemon` on the next herdr start. To run
it in an already-live session:

```sh
nohup wsp daemon > ~/.local/state/wsp/daemon.log 2>&1 &
```

Sidebar rows go in `~/.config/herdr/config.toml` — see `[ui.sidebar.spaces]` and
`[ui.sidebar.agents]` there; `$proj`, `$todo`, `$doing`, `$blocked` and `$task`
are published by the daemon.

## Layout

| Path | What |
|---|---|
| `~/wsp/projects/<slug>.md` | one project per file: parent, tags, roots, brief |
| `~/wsp/tasks/<id>.md` | one task per file: `t-YYMMDD-NNN` |
| `~/wsp/archive/tasks/YYYY-MM/` | swept `done` tasks |
| `~/wsp/hooks/on-<event>` | executables fed event JSON on stdin |
| `~/.local/state/wsp/` | claims, bindings, pins, mandates, `worked.json`, `events.jsonl` — machine-local, not in git |

Override the store with `WSP_HOME`, state with `WSP_STATE`, and disable
autocommit with `WSP_NO_COMMIT=1`.

## Everyday use

```sh
wsp add "Port the reverb fix" -p trance --prio high
wsp ls                      # open tasks for the project you're standing in
wsp inbox                   # tasks with no project
wsp start 003               # ids accept a bare suffix or a title substring
wsp block 003 "waiting on the tuning table decision"
wsp done 003
wsp tree                    # hierarchy with rolled-up counts
wsp wip                     # every agent, its task, and who needs you
wsp where                   # what project am I in, and why
wsp overlap                 # who else is standing in this tree
```

## The panel

`wsp panel` is a sidebar that runs in a pane of its own. herdr's own sidebar
lists workspaces and hangs agents beneath them; this inverts that — the spine is
the project tree, tasks hang off projects, and panes hang off whichever they
belong to: the task they claimed, or failing that the project they are standing
in. Tasks belonging to no project get an `inbox` heading at the top; panes
belonging to no project get a `no project` group at the foot.

Panes come from `pane.list`, not `agent.list`. Most panes are a shell nobody is
driving, and a shell sitting in a project is a fact about that project — the
earlier version asked for agents only and so could see one pane out of
twenty-two.

```sh
wsp panel install            # split it into the workspace you're standing in
wsp panel install --all      # …or every one of them
wsp panel uninstall
```

Both the panel and the view watch their own binary and re-exec when it changes,
so `install -m 755 target/release/wsp ~/.local/bin/wsp` reaches every open pane
within a tick. `exec` rather than a respawn: the pane, its pty and its place in
the layout all survive. Without it, twenty-two panes sit holding a stale image
and a key silently does what it used to do — which is worse than one that errors.

Install splits with `pane.split` and swaps the new pane into the narrow slot.
It must never go back to `layout.apply`: herdr rebuilds the whole tree from
that call and every pane in it gets a fresh terminal, which takes down any
agent running in the workspace.

Keys: `j`/`k` move, `←`/`→` fold, `↵` opens the row in the detail pane (and
closes it again), `esc` closes it, `E` pops the row's file out into an editor
tab, `1`-`9` jump straight to an agent, `A` shows finished tasks, `i` shows
ids, `r` syncs, `?` opens the key map.

`i` puts each task's id in front of its title — the thing you type at a shell
beside the thing you read. Off by default: `t-260815-004` is thirteen columns of
a pane that is thirty wide and eleven of them are identical on every row, so
what shows is the suffix, which is exactly what `wsp start 004` resolves. When
another *open* task shares that suffix the date is what separates them and the
whole id appears instead; a finished task always shows in full, because a bare
suffix resolves against open tasks only and an id you cannot type is worse than
no id at all.

`q` and `esc` both mean "put away what is in front of me": the key map first,
then the detail pane. Neither quits the panel — it is installed furniture in
every workspace, so losing one to a stray keystroke costs a reinstall and buys
nothing. `ctrl-c` still quits, and `wsp panel uninstall` is the deliberate way
out.

`?` docks the key map under the tree, and `?` or `esc` puts it away — a footer
line fits four of the two dozen keys, which is worse than showing none, because
it says there is a list and then hides most of it.

The map takes the rows it needs and no more, and **nothing else changes while
it is up**: the cursor keeps its row and every key still does what the map says
it does, so you can read `b` and press it on the task you were already looking
at. The tree simply has fewer rows to work with, and it goes on holding the
selection near the middle of them rather than letting the map push it off the
bottom. That centring is how the tree scrolls generally — a cursor parked on
the last visible row shows you everything you have walked past and nothing you
are about to reach; only the two ends of the list break it, where there is
nothing further to show.

The verbs are listed first because a pane too short for the whole map cuts from
the bottom, and movement is the half you can find by pressing an arrow and
watching. The footer says how many lines it could not fit.

## The detail pane

`↵` on a task or a project opens it in a second pane, split beneath the panel.
The panel answers *what is there*; this answers *what is this* — a task's
resolved tags, its claim, the pane working it and its log newest-first; a
project's rolled-up work, what sits under it and its own tasks.

```sh
wsp view            # follows whatever the panel last opened
wsp view <id>       # pinned to one thing
```

There is one view pane per workspace and `↵` retargets it rather than stacking
another, so reading a second thing does not move the pane you are reading. The
target passes through a file the view polls: retargeting costs no process
churn, where killing and relaunching would blink the pane on every keypress.
`↵` on a pane row still jumps to that terminal — a pane's detail *is* the
terminal. `↵` again on whatever is already open closes the pane, as does `esc`,
so the same key both opens and closes and there is nothing to remember.

For real editing, `E` pops the row out into a **tab** of its own:

```
┌─ context (live) ───────────────────────┐
│ status · claim · log                   │
├─ overview ──────────┬─ details ────────┤
│ prose, no markup    │ prose, no markup │
└─────────────────────┴──────────────────┘
```

The context is the same live view the sidebar opens, so status, claim and log
keep updating while you type — that context was exactly what editing in a bare
buffer cost. Each section gets its own editor on its own buffer, containing
prose and nothing else: there is no `##` left to mangle. They are safe to run
together because `wsp edit` re-reads the task and writes back only its own
section. The tab opens with the **context** focused, not an editor — so `W` and `q` are
under your hands before you have committed to typing anything. From there `h`
and `l`, or `←` and `→`, go to the pane on that side of the screen.

By position, not by name: `o` for overview and `d` for details put the key for
the *left* pane under the right hand, which is backwards every time you reach
for it. `h` and `l` are left and right on the keyboard, in vim, and in herdr's
own `prefix+h/l`. The side is resolved from herdr's actual geometry rather than
from the layout this code happens to build, so it stays true if that changes.

Getting back, and moving around generally, is herdr's own: `prefix+h/j/k/l`
focuses the pane left/down/up/right, where the prefix is whatever
`[keys] prefix` says in `~/.config/herdr/config.toml`. There was no reason to
reinvent that; `o` and `d` exist only because naming a pane you are looking
straight at beats a two-step reach for it.

`W` in the context pane saves and closes both editors at once. It sends two
things: an abort — `Ctrl-C` for the vi family, `Ctrl-G` for emacs — and then
the save-and-quit, `:wqa` or `^O ^X` or whatever that editor wants. `vi` is
assumed when `$EDITOR` is unset, and an editor it does not recognise is named
rather than guessed at.

Both halves earn their place. The abort clears insert mode, a half-typed `:`
command, a pending operator, a *Press ENTER* prompt, and `q:` — which opens
vim's command-line window, where `Esc` does nothing and `:wq` is just text. And
they go as **separate writes**, because vim discards pending type-ahead when it
takes an interrupt: send both together and the command is eaten, leaving
exactly the stuck pane this is meant to prevent. The quit is `wqa`, so a split
made inside the editor does not leave the pane behind.

If a pane still will not close, `W` says which, and pressing it again closes
them outright.

`q` is the opposite of `W`: it tells the editors to quit *without* saving and
closes the tab, whether or not they go quietly — a pane that will not quit must
not be able to veto a decision to be rid of the thing. In a view opened from the
sidebar there is no edit tab to close, so `q` closes only itself; closing that
tab would take the panel and whatever you were working in with it.

Quitting the editors by hand works the same way: the second one to go takes the
tab with it, and quitting the first leaves it standing because closing then
would take the other's work.

Each editor is labelled by its section, and its buffer is a file named
`overview.md` or `details.md` — so the editor's own status line says which half
you are in, without the editor having to cooperate.

Closing the tab from herdr while an editor is open **loses that editor's
changes**: `wsp edit` writes back only after the editor exits. If you had saved,
the staged file survives under `$TMPDIR/wsp-<id>-<stamp>/`. A tab rather than a split: a task's whole body —
notes, acceptance criteria, the log — wants width, and a tab gives it without
disturbing a layout you will come back to.

### Managing from the panel

| Key | On | Does |
|---|---|---|
| `a` | project, inbox | add a task in that scope |
| `a` | task | add a **sub-task** of it |
| | | the cursor lands on it, so `E` writes it up |
| `P` | anywhere | new project, child of the selected one |
| `s` `v` `d` `o` | task | start, review, done, reopen |
| `b` | task | block, asking why |
| `e` `n` | task | retitle, append a note |
| `E` | task, project | edit its prose full-screen in a tab |
| `m` | task | move — the tree becomes the picker |
| `c` | task or agent | claim, either direction — and how an agent moves on |
| `O` | task, project | open a herdr workspace for it, and claim it |
| `X` | task, project | remove, after a `y`/`n` |

Nothing is reimplemented here: a key builds an argv and the panel runs its own
binary, so the event log, the hooks and the git commit all happen because it is
the same path a person at a shell takes. `wsp rename`, `wsp rm` and
`wsp project rm` exist because the panel needed them — `edit` opens `$EDITOR`,
which is no use to something already drawing on the screen.

`O` creates the workspace rooted at the project's root, labelled after the
work, with `WSP_PROJECT` and `WSP_TASK` in its environment — so every pane
inside it knows what it is for instead of the cwd having to imply it. herdr
does not persist env across a restart, which is why the durable record is the
claim; the env is exact for the life of the session.

Typing, picking and confirming are modes, not widgets: navigation and folding
keep working inside a pick, so you hunt for a destination by reading the tree.

### What the marks mean

| On a task | | On a project | |
|---|---|---|---|
| `·` | todo | `▾` `▸` | unfolded / folded |
| `▸` | doing | `7` | open tasks, rolled up |
| `■` | blocked | `▸3` | tasks in flight |
| `◆` | in review | `■1` | tasks blocked |
| `✓` | done — only under `A` | `✓` | all work here is finished |
| | | `●2` | panes standing here |

A pane gets its own row: `●` working, `○` idle, `▫` a shell with no agent in it
— never started, as against an idle agent that stopped. A task keeps its own
status glyph even when claimed, because the pane is on the row beneath rather
than borrowing the task's.

`←` marks an idle agent on a task that is still `doing` — it has stopped and
you are the blocker; the header carries the count. `⋯ n more` is the tail past
the six-task cap, which counts top-level work only: a sub-task is never hidden
while its parent is on screen.

A task with work under it carries the same right-hand counts a project does —
open, `▸` in flight, `■` blocked, `✓` when everything beneath it is finished.
Same numbers, one level down.

The inbox sits at the top: unfiled work is what you triage before reading
anything that already has a home. A `no project` group sits at the foot for
panes that resolved nowhere. Both fold like a project and take the cursor like
one, so a command can be aimed at them.

### The dock

Under a rule at the very bottom sits **`unassigned`**: every agent holding no
task, wherever it is standing. It is pinned — the tree scrolls above it and
this does not — because it is the row you most need to see and the first the
tree would push off the bottom, sorting as it does by work these panes have
none of. `c` on one turns the tree into the picker and gives it something to
do.

Only agents. A shell with nobody driving it stays in the tree, under the
project it is standing in or in `no project`: it is a fact about a place. An
agent with no task is a person's worth of attention going spare, which is a
fact about *you*.

The dock takes the cursor like any other row, which is why its rows live at the
end of the same list rather than being drawn separately — a block you can see
and not select would show you an idle agent and give you no way to act on it.

Colour carries six roles: **bold** is a project, plain is a claimed task, muted
is an unclaimed one, dim is structure and finished work, accent is live work,
and warn wants a decision.

Note `▸` does double duty — a folded caret on the left of a project row, a
count of tasks in flight on the right.

### Storyboard

`wsp panel storyboard [--out page.html]` renders the panel offline: fixtures for
layout, and flows that push scripted keys through the same reducer the live
panel runs. No herdr, no store, no terminal — useful for arguing about the
design before building it. The legend above is generated from the same glyph
constants and `Style` values the rows draw with, so it cannot drift.

## What a task or project file holds

Frontmatter is a contract — `id`, `status`, `schema` — and every field in it has
a command that sets it correctly. The body is yours, and carries four sections:

| Section | For |
|---|---|
| `## Overview` | what the task is, written once, read to re-enter it |
| `## Details` | working material — criteria, links, whatever the work needs |
| `## Decisions` | what was settled and now binds; `wsp decide` writes it |
| `## Log` | dated, append-only; `wsp note` writes it, nothing edits it |

```sh
wsp decide 022 "parked rather than dropped — the mechanism is right, the moment is not"
wsp decide wsp "the backlog is split into render and data, and here is what that means"
```

A decision is not a task and not a note. A task is work that completes; a
decision never does, so filing one as a task leaves it open in every list for
ever. A note lives on one task, where a decision that binds a whole project
cannot be found by the agent who needs it — which is the agent about to do the
thing the decision rules out. So `wsp decide` takes either a task or a project:
the same sentence, at the height it applies.

It is not a file beside the code, either. `ARCHITECTURE.md` and its kind stay in
the repo, because they describe the implementation and travel with it. A
decision is about the *work*, and work lives in the store.

Dated and append-only, like the log, and for the same reason: there is no `wsp
undecide`. A decision that turns out wrong is superseded by a later one saying
so, which is the honest record — what a reader three months on needs is the
reasoning that was live at the time, not a tidied conclusion. It sits above the
task list in `wsp project show`, because it is a constraint on what may be
picked up next and belongs in front of the list of things somebody might pick.

```sh
wsp edit <id>                    # both sections, headings included
wsp edit <id> --overview         # just that prose
wsp edit <id> --details
wsp edit <id> --raw              # the whole file, when the frontmatter is wrong
wsp project edit <id> [--overview|--details|--raw]

wsp edit <id> --overview -       # …or from stdin, for something without hands
wsp edit <id> --details --from notes.md
```

`$EDITOR` is the answer for a person and no answer at all for anything else. An
agent that can only append to the log writes tasks with a title and nothing
under them, which is most of what makes a decomposed task unreadable a day
later — so the same prose can arrive from a file, or from a bare `-` meaning
stdin, spelled the way `cat -` spells it. It takes the same path back: the task
is re-read first, only the section asked for is carried across, and `## Log`
stays out of reach. Whitespace at the end is not an edit, or an agent rewriting
the same brief would touch the task and make a commit every time.

`--raw --from` is refused. `--raw` is the one path that reaches the
frontmatter, and the point of `--from` is that nobody is reading what goes
past: a generated file landing on `status:` is not an edit, it is a task the
tools can no longer read.

Projects carry the same two sections and take the same editors — `roots`,
`tags`, `parent` and `status` all have `wsp project set`, so there is nothing
in a project's frontmatter an editor needs to reach either. A row with anything
written in it is marked `≡` in the tree, because the point of writing it down
is being reminded it is there.

`wsp edit` re-reads the task after the editor exits and carries across only the
section you edited. An edit lasts as long as someone is typing and the file is
shared — a note, a status change, a claim — so writing back the copy it opened
with would silently undo whatever happened meanwhile.

`wsp edit` never puts the frontmatter in front of you. A typo in `status:` is
one keystroke from a task the tools can no longer read, and there is nothing to
gain by allowing it — the part worth writing by hand is the prose. Sections come
back in canonical order, headings you invented are kept, and if you delete both
headings and type anyway your text is filed under Overview rather than dropped.

## Adopting what is already open

```sh
wsp pin --top -w w0     # this workspace is not work; keep it out of the tree
wsp adopt               # what would be captured
wsp adopt --yes         # make the tasks and claim them
```

A workspace carries its meaning in a hand-typed label — `Trance Video`,
`TET -> EIN` — and nowhere else. `adopt` reads those, makes a task per
workspace in whichever project the *label* points at, and claims it there.
Label first is deliberate: ten workspaces share `~/claude/vst`, so cwd
collapses them all onto `vst` and only the label separates trance from ein
from verb. Workspaces whose label is just the folder name are skipped, as are
ones already claimed and ones pinned `--top`.

Inside a herdr pane, `wsp claim <id>` binds the pane to a task (via
`$HERDR_PANE_ID`), which is what makes `wsp wip` and the `$task` sidebar token
work. `wsp release` unbinds; `pane.exited` does it automatically.

## What an agent is handed

```sh
wsp brief            # one call, for a session-start hook
wsp brief --json
```

```
where  meta/tooling/wsp  rust herdr
you    t-260815-005  doing  wsp brief: what an agent is handed at session start
under  t-260815-003  Claude Code lifecycle: agents that keep the store themselves
open   t-260814-023  blocked   Storyboard: scripted input through the reducer…
       t-260815-003  todo    ! Claude Code lifecycle…                   (8 open)
       4 more · wsp ls
here   wsp           Atomic claim, and refuse to take a task…   same tree · 26s
       vst           (Evaluate infinite canvas performance …)  same tree
others 20 more · wsp overlap
```

Every line of it is answerable already — `where` for the project, `ls` for the
backlog, `wip` for the other agents. One command exists because a session-start
hook can afford exactly one call, and because what it prints is a *briefing*
rather than a report: the few facts an agent cannot work correctly without, in
the order it needs them, short enough that nobody turns it off to save context.

`here` is the line that would have caught two agents editing one checkout this
morning. It comes from the same `standing_beside` reckoning `wsp overlap` and
`wsp claim` use, split at `Relation::is_near()`: panes that can reach the files
under your hands get named and coloured, everyone else is context. The far set
names whoever is holding something and counts the rest — twenty of twenty-two
panes here are shells that have sat in a directory since Tuesday, and naming
them would push the two that matter off the bottom.

`decided` carries what is already settled, and sits above the backlog rather
than below it: a decision is a constraint on what may be picked up, so it
belongs in front of the list of things to pick. It reads the whole project
chain — a decision made on `wsp` binds work taken in `data`, exactly as a tag
does and for the same reason, because the work is inside it.

`under` is there because direction lands on a parent and the work happens a
sub-task at a time, so the piece in hand is rarely the reason it is being done.
A sub-task whose parent is also on the `open` list is left out of it — the
parent's count already speaks for it, and printing both spends the cap twice on
one piece of work.

It never fails. An empty store, a herdr that is not answering, a pane belonging
to no project: each of those is a shorter brief, not an error. A hook that
errors on a fresh machine is a hook people delete.

### Who else is standing here

```sh
wsp overlap          # who can reach the files under my hands
wsp overlap --json
```

```
2 in this tree  — they can reach the files you are editing
    wP:p3    wsp brief: what an agent is handed at session start  same tree   7m
    wW:p3    (◑ Evaluate infinite canvas performance on MacBook)  same tree

20 idle shells in other trees · wsp wip
```

The question is *where a pane is standing*, not what it claimed. On 2026-08-15
three agents worked this repo at once, none knew, and one swept another's
uncommitted work into a commit — and the pane that did the damage held no claim
for the first twenty minutes of its life. A report that needs a claim to fire
would have been silent through exactly the window that cost us. A claim makes
the answer better; it cannot be what triggers it.

So panes are ranked by how close they are: the same tree (one directory inside
the other), the same checkout (siblings under one project root), the same
project reached another way, then everywhere else. Only the first two are a
warning, because only they can overwrite a file you have open. herdr reports
where a pane's *shell* started rather than where its agent is working, which is
why containment counts and not just equality — an agent launched from `~/claude`
is working in `~/claude/wsp` soon enough.

A pane with no agent counts. A shell is a fact about the tree, and a person at
one can clobber a file as thoroughly as an agent can. Our own panels and view
panes do not: they are in every workspace by construction, and counting them
would bury the row that matters.

`wsp claim` says the same thing unprompted, since claiming is the moment an
agent commits to a tree and the cheapest moment to learn it is not alone in one.
`wsp brief` carries it into the session-start hook. All three read the one
definition in `src/overlap.rs`, so there is no second answer to drift.

### The standing rules

`~/wsp/agents.md`, if it exists, is printed at the end. The protocol an agent
works to belongs in the store rather than compiled into this binary: it is
yours to edit, versioned in git beside the tasks it talks about, and readable
by anything that can read a file. A rule nobody can change without a rebuild is
a rule that goes stale.

It is also the only place the *trigger* can live. A skill loads on demand,
which is no use for behaviour that has to happen without being asked — so the
few sentences that say when to reach for the CLI have to be in front of the
agent before it does anything, and this is the file the hook puts there.

## Sub-tasks

```sh
wsp add "Wire the daemon to the socket" --parent 005
wsp done 005                 # refused while anything under it is open
wsp done 005 --force         # …unless you say so
```

A task can sit under another. It is one field, `parent`, and it exists because
work arrives at one size and gets done at another: you say *design the workspace
management system*, and what actually happens is six things, each of which wants
its own status, its own claim and its own log.

That makes the sub-task the unit an agent can be given. Direction lands on the
parent; the pieces beneath it are what gets claimed, worked and finished, and
because a pane holds one current task the parent accumulates a legible trail of
who did what and in what order.

The rules are the few that keep the tree honest:

- **A sub-task lives where its parent lives.** `--parent` takes the project
  from the parent, and naming a different one is refused rather than filed —
  one piece of work in two places in the tree is a piece of work you lose.
- **A parent cannot be finished while its children are open.** The refusal
  names the count; `--force` overrides it, and from the panel `d` turns the
  refusal into the next question rather than swallowing it.
- **A task cannot be its own parent.** `--parent` is resolved before the new id
  is allocated, because id allocation reserves the file first — so `--parent
  004` used to be able to match the very task being created. `doctor` reports
  cycles as well as a missing parent, since a loop resolves at every step.
- **Filtering never drops a child.** A task whose parent is not in the list is
  drawn at the top level rather than hidden; a row indented under nothing is
  just a row that looks broken.

## Standing direction

```sh
wsp mandate wsp          # work here without being asked again
wsp mandate              # what is the standing direction?
wsp mandate --clear      # give it back
```

A claim says what an agent is doing now. A mandate says what it is *for* —
which is the question an agent has to answer for itself every time it finishes
something. Without one, a session records faithfully what it was told and then
stops. With one, `wsp brief` opens with it and names the task to take next:

```
mandate wsp   take work here without asking
you    nothing claimed
next   t-260815-009  review is the agent's terminal verb  wsp claim it
```

It lives beside the claims, keyed on the workspace and the host, and it
survives a restart — direction you have to repeat every morning is not standing
direction, it is a reminder. In the resolution chain it sits at **pin > binding
> mandate > cwd > label**: a pin says what a workspace *is* and a binding is the
work actually in hand, but a mandate beats the directory a shell happens to be
sitting in. It is deliberately not consulted when the panel places a pane in the
tree — where a pane is standing is a fact about the pane, and standing direction
says nothing about it.

Scope is the project and everything under it, and containment reads **both
ways** along the chain: a mandate on `data` while standing in `wsp` is in scope,
because `data` declares no roots of its own and so can only be worked from
inside its parent's. Reading it one way round would have called that out of
scope and passed every test anyone would think to write, since the obvious test
is a mandate on a project that has a root.

The limits are the design rather than a caveat. Work still finishes at
`review`, never at `done`; anything needing a decision is still `wsp block`ed
with the question rather than guessed at; and the mandate ends — on `--clear`,
or when the backlog runs dry, which the brief says in as many words. A mandate
with no end is standing permission, which is not what "go work on project x"
means.

## Moving between tasks

One agent works several tasks in a sitting, so `claim` is also the verb for
moving: claiming a second task hands off the first rather than quietly leaving
it claimed. From the panel it is `c` — on an agent row, pick the task it moves
to; on a task row, pick the agent that takes it.

```
▸ t-260814-026  Panel management: keys for tag, pin, priority
  bound to w5:p1
  left t-260814-025  Agent migration between tasks
```

**One pane, one current task.** Not a queue: the project's task list is already
the plan, and every join wsp makes — `wip`, the `$task` token, which task a pane
hangs under in the tree — needs one answer to *what is this agent doing now*,
not a list and a rule for picking from it.

**The task being left keeps its status and loses its claim.** `doing` with
nobody on it is a real state — it is the work that is underway and waiting for
you, which is exactly what `wsp wip` flags. What it must not keep is the claim,
or `reconcile` puts the agent back on it after a restart and `adopt` goes on
treating its workspace as spoken for. `done` releases the claim for the same
reason: work that is finished should not still be holding a workspace.

**What it keeps instead is the record.** A line in its own log — `handed off to
t-260814-026 after 3h12m` — which is durable, readable and in git; and
`worked.json`, which is the same fact structured, so `wsp show` and the detail
pane can say where it was worked, for how long, and what it went to:

```
worked    panel work · 3h12m · to t-260814-026
```

That file sits with the claims, machine-local, because a workspace id and a cwd
mean nothing on another host. Both halves of a claim now end the same way,
whether it was migrated, released or finished.

**Two panes never hold one task.** Claiming work that another agent has takes
it off them, and says so on the task. The tree hangs a pane under the task it
is bound to and draws the first it finds, so the second was never visible —
it was a state you could reach and not see.

**But not off a *live* one, and not by accident.** Taking work from a pane that
still has an agent in it is refused:

```
✗ t-260815-004  Sub-tasks: make the parent field real
  held by claude in wP:p3 · working · 41m
  wsp claim t-260815-004 --force   to take it anyway
```

It used to be silent. The binding was cleared, the other agent went on editing
files for a task the store had given away, and the first anyone knew was two
commits fighting over the same lines. A *dead* pane's binding is a different
matter and is still taken without asking — that stale state is exactly what a
re-claim is for.

Claiming something already `done` is refused the same way, because it silently
reopens it: the status goes back to `doing` and the task rejoins every open list
on the machine. That is occasionally what you want, and never by accident — a
bare `005` resolving to finished work is how the accident happens. This one was
found by making it: the command was pointed at a completed task while testing
the refusal above, and quietly undid somebody else's morning.

Both refusals happen **before anything is written**, so a refused claim costs
nothing — the agent has not yet let go of whatever it was holding.

**The row moves; the cursor goes with it.** A claim re-sorts the tree under
your hands — the pane row leaves one task and reappears under another, often
several lines away. The panel holds the cursor on the row it was on rather than
the position that row was in, so the eye keeps the thing it was following.

Every command takes `--json`.

## Source map

| File | Responsibility |
|---|---|
| `src/main.rs` | argument parsing, dispatch, help |
| `src/store.rs` | atomic writes, `O_EXCL` id allocation, git, state, hooks |
| `src/fm.rs` | the small YAML-frontmatter subset |
| `src/model.rs` | `Project`, `Task`, status/priority vocabulary |
| `src/resolve.rs` | project resolution, tag inheritance, count rollup |
| `src/herdr.rs` | newline-delimited JSON-RPC over herdr's unix socket |
| `src/sync.rs` | tasks + panes → metadata tokens |
| `src/daemon.rs` | event subscription, debounce, TTL refresh |
| `src/input.rs` | terminal bytes → keys: the escape-sequence parser |
| `src/panel/rows.rs` | what is in the tree, and how each row draws |
| `src/panel/render.rs` | `Line`/`Style`, the frame, and the ansi + html backends |
| `src/panel/keys.rs` | modes, movement, and the map |
| `src/panel/verbs.rs` | what the letters do |
| `src/panel/run.rs` | the terminal, the event loop, the effects |
| `src/panel/install.rs` | splitting the panel into a workspace, and back out |
| `src/detail/render.rs` | a task or a project, in full |
| `src/detail/editors.rs` | getting the editors a pop-out opened to go |
| `src/detail/run.rs` | the detail pane itself |
| `src/cmd_brief.rs` | one call for a session-start hook: where, what, who else |
| `src/cmd_mandate.rs` | standing direction: what a workspace is for |
| `src/cmd_*.rs` | the commands |

The panel is split where the *work* splits rather than by layer: a row's data
and its drawing sit together in `rows.rs`, because a field added to a row is
read where it is written and drawn a hundred lines below, and separating them
would put one change in two files every time. What `render.rs` keeps is the
surface a row is drawn onto, which nothing about a row needs to know.

Dependencies: `serde_json`. That is the whole list, and it should stay that way —
fast builds are a feature here, because a session-start hook runs this binary.

## Notes

- **Status is work state, not process state.** herdr's `idle`/`working` describes
  the process; `doing`/`blocked`/`review` describes the work. `wsp wip` flags the
  gap between them — process-idle on a `doing` task means a human is the blocker.
- **The focused panel refreshes at 250 ms; the rest at 30 s.** Both the store
  stat and the two socket calls sit behind that gate, so twenty idle panels do
  no work between refreshes while the one you are looking at feels immediate.
  Rendering a frame costs 0.33 ms and herdr answers in 0.6–5.6 ms — the lag was
  never compute, it was a five-second tick.
- **Claims outlive panes; bindings do not.** A binding is keyed on a pane id,
  the most perishable thing herdr has. A claim names the workspace — id, label
  and cwd, all of which herdr persists — and is cleared only by `release`. A
  pane exiting leaves the claim standing, and `wsp reconcile` rebuilds the
  bindings from claims against whatever is currently open. The daemon does it
  before its first sync, which is when herdr has just restored everything under
  new pane ids. One pane takes one claim there too: two claims naming the same
  workspace used to land on the same pane, and since claims are walked in id
  order the agent came back bound to the *older* task — the one it had left.
- **cwd is not identity.** Five workspaces share `~/git/Easter`. Resolution order
  is pin → binding → cwd → workspace label, so `wsp pin <project>` is the
  override when a directory is ambiguous.
- **The archive never overwrites.** It files by id, so an id handed out twice
  put the second task straight on top of the first: four tasks shared one
  archived file here before anyone noticed the ids were being reused at all,
  three of them recoverable only from git. A name already taken now gets a
  `~2`, and `wsp rm` says so. Ids are unique going forward; an archive that can
  destroy the record it exists to keep should not be one bug away from it.
- **An id is never handed out twice.** `t-YYMMDD-NNN` is allocated past
  everything the day has already used, live *or* archived — not into the first
  free filename. Archiving moves a task out of `tasks/`, so the old probe gave
  the next task the name of the one just retired: two pieces of work answering
  to one id, with the log, the claim, the ghost and any `parent` pointing at it
  describing both. `O_EXCL` still settles two agents adding in the same second;
  what changed is where the count starts. `wsp doctor` reports a live id the
  archive also holds, and names the claim or worked record left keyed on it.
- **Tags are inherited.** A task in `trance` also matches `-t juce` and `-t dsp`
  from `vst` and `audio` above it.
- **One writer at a time in `~/.local/state/wsp`.** Every state file is read,
  changed in memory and written back whole, so `write_atomic` making each
  *write* indivisible was never enough: two agents claiming at the same moment
  both read the old map, both insert their key, and the second write drops the
  first. Measured on this machine before the fix, thirty-two concurrent writers
  left **two** records standing; after it, thirty-two. The window is a
  millisecond wide, which is why it read as "herdr lost my claim" rather than as
  a bug. A lock file around the whole read-change-write cycle closes it. It is
  reentrant, so a claim can hold it across the several files it touches, and it
  gives up after two seconds rather than hang — a lost update is recoverable,
  a `wsp claim` that never returns is not. herdr calls stay outside it: a
  socket round-trip is not something other agents should queue behind.
- **Every escape sequence is consumed whole.** Reading a fixed two bytes after
  `ESC` matches the four arrows and leaves the tail of everything else in the
  buffer, where the next read hands it over as typing: page-up put a `~` in a
  task title, and ctrl-up put `;5A`. `src/input.rs` follows the grammar
  instead — parameters, intermediates, final byte, the string sequences a
  terminal replies to a query with, and the three bytes an X10 mouse report
  hides *after* its final. A sequence nothing here answers to is dropped in
  silence rather than spilled. Bare `ESC` is the one thing bytes cannot settle,
  so `stty min 0 time 1` settles it: a read that comes back empty means there
  was nothing behind it, and it was the key.
