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
wsp spawn 003 --agent       # open a workspace on it and put an agent in it
```

## The panel

`wsp panel` is a sidebar that runs in a pane of its own. herdr's own sidebar
lists workspaces and hangs agents beneath them; this inverts that — the spine is
the project tree, tasks hang off projects, and panes hang off whichever they
belong to: the task they claimed, or failing that the project they are standing
in. Tasks belonging to no project get an `inbox` heading at the top; shells
belonging to no project get a `no project` group at the foot, and the agents get
a section of their own, pinned under a rule at the very bottom.

Panes come from `pane.list`, not `agent.list`. Most panes are a shell nobody is
driving, and a shell sitting in a project is a fact about that project — the
earlier version asked for agents only and so could see one pane out of
twenty-two.

```sh
wsp panel install            # split it into the workspace you're standing in
wsp panel install --all      # …or every one of them
wsp panel uninstall
```

The panel, the view and the daemon all watch their own binary and re-exec when
it changes, so `install -m 755 target/release/wsp ~/.local/bin/wsp` reaches
every long-lived process within a tick. `exec` rather than a respawn: the pane,
its pty and its place in the layout all survive. Without it, twenty-two panes
sit holding a stale image and a key silently does what it used to do — which is
worse than one that errors.

The daemon was the last one to get this, and its absence was quieter than the
panel's would have been. It has no screen to look wrong: it goes on answering
and syncing, on whatever binary it happened to start with. The one running when
this was written had been up for a day and had sat through two installs, so a
store fix shipped that morning was live in every panel and absent from the
process that polls the store hardest. It re-execs at the top of its loop, where
no sync is in flight and no lock is held, and it carries `-v` across — a daemon
that went quiet the first time it reloaded would look exactly like one that had
died.

Install splits with `pane.split` and swaps the new pane into the narrow slot.
It must never go back to `layout.apply`: herdr rebuilds the whole tree from
that call and every pane in it gets a fresh terminal, which takes down any
agent running in the workspace.

Keys: `j`/`k` move, `←`/`→` fold, `↵` opens the row in the detail pane (and
closes it again), `esc` closes it, `E` pops the row's file out into an editor
tab, `1`-`9` jump straight to an agent, `A` shows finished tasks, `R` narrows
to what needs reviewing, `w` shows the agents instead of the work, `i` shows
ids, `r` syncs, `?` opens the key map.

### The strip, and the agents view

The top line carries one mark per running agent, and the same marks are the
first column of the agents view `w` opens. herdr reports two states — working,
or idle — and `idle` is an answer to a question nobody asked: an agent that has
stopped is waiting for *something*, and which something decides whether you
have to get up. The store holds the other half, so the two become four:

| Mark | What it is waiting for |
|---|---|
| `←` | stopped, holding a task that is still live — you are the blocker |
| `■` | stopped, on a task parked with a question written on it |
| `●` | running |
| `○` | spare — stopped and holding nothing at all |
| `·` | herdr says neither, usually a pane that has not spoken since it started |

What wants you sorts first and what is free comes next, in both places: there
is nothing to do about an agent that is working, and sorting the busy ones above
the spare one is how the row you were looking for falls off the end of the five
the panel keeps on screen. The strip is drawn from every agent on the machine
whatever the tree is filtered to — a header that went quiet under `R` is one you
learn to distrust — and the total stays on the right, because a narrow pane
clips the strip and a clipped strip must not be the only thing saying how many
there are.

**Each mark is clickable** and goes straight to that terminal — one click, not
the select-then-activate a row gets: a mark is a single column with nothing to
read on the way, and the `←` you are reaching for is the one you have already
decided to answer. Clicking the `⋯` of a clipped strip opens the agents view,
which is what the rest of them are.

Beneath the tree, under a rule of its own, the first five agents are pinned in
that same order: who has stopped and who is free is the question you ask between
reading anything else, and it should not be a keystroke away. The heading counts
them all, so a sixth is never silently absent; `→` on its `⋯` opens the tail in
place, and the digits `1`-`9` start here rather than in the tree, because a
digit you can always see is worth more than one in row order. A pane drawn twice
— under its task and again in the section — spends only one of the nine.

`w` gives the same agents the whole pane and three lines each: what it is
waiting for in words, which terminal it is, how long it has held what it holds,
where it is pointed if that is not where it stands, and the task itself. Only
the first line takes the cursor; the two beneath it are that line said at
length, and a click on either lands on the agent they belong to. It is
deliberately not a filter over the tree: the tree is ordered by what has to be
done, and an agent with nothing to do has no work to be filed under. Every row
is a pane, so `↵`, `c`, `f` and `1`-`9` all go on meaning what they mean — which
is the point, since you open it to find who is free and end up already standing
on the row that hands them something. Shells are left out of both: a pane with
nobody in it is a fact about a place, and this is a list of people. `w` and `R`
each put the other away.

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
│ 1 overview  2 details   ·   D decisions│
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

#### Three sections, three columns

The menu across the context pane is the layout, written down. On the left, the
columns you have, numbered — the number is what you type. On the right, after a
separator, whatever is not on screen, with the key that would fetch it. A
section is never in both halves; telling those two states apart is the whole
value of the line.

```
1 overview  2 details   ·   D decisions
```

Two keys, and they compose:

| Press | Means |
|---|---|
| `1` `2` `3` | how many columns there are |
| `o` `d` `D` then `1` `2` `3` | put that section in that column |

So `d 2` is details in column two, `D 3` is decisions in column three, and a
bare `3` is all three sections at once. The digits mean two different things
depending on what came before them, which is exactly why it is a chain rather
than six keys: `2` on its own is a question about the layout, and `d 2` is a
question about one column.

A half-typed chain is not a trap. `d` then anything that is not a digit cancels
and says so, and the key you actually pressed is then handled normally — so
changing your mind mid-chain still saves, still scrolls, still quits.

The rules are the few that keep the columns honest:

- **No section is ever in two columns.** Two editors on one buffer means the
  second to exit wins and the first person's typing is gone — `wsp edit` reads
  and writes back a whole section. So placing a section that is already on
  screen makes the two columns **trade** rather than overwriting the target.
- **Asking for a column past the end grows to reach it.** `D 3` from a
  two-column tab is unambiguous, and refusing it so you can press `3` first
  would be pedantry.
- **Shrinking drops from the right, growing appends what is missing.** `1` then
  `3` gets you back something recognisable rather than the default: the column
  you were writing in stays put, and only the ones you asked about move.

Those rules live in `Columns`, apart from any pane, because everything else
here needs a terminal and a herdr to exercise and that does not. A key press is
a function from one `Columns` to the next; moving panes is the diff between
them, applied **grow, then re-point, then shrink** — a section moving out of a
column that is about to close has to reach its new home before the old one goes.

#### Moving between the panes

`h` and `l`, or `←` and `→`, focus the pane on that side of the screen — by
position, not by name, because `h` and `l` mean left and right on the keyboard,
in vim, and in herdr's own `prefix+h/l`. The side is resolved from herdr's
actual geometry rather than from the layout this code happens to build, so it
stays true if that changes.

That same geometry is what orders the columns the menu numbers. herdr lists
panes in creation order, which stops meaning "left to right" the first time a
column is closed and another opened — which, with a menu that adds and removes
them, is immediately. Reading the order back off the layout each time costs one
call and cannot go stale; remembering it in the context pane would describe a
column that somebody had since quit by hand.

Moving around more generally is herdr's own: `prefix+h/j/k/l` focuses the pane
left/down/up/right, where the prefix is whatever `[keys] prefix` says in
`~/.config/herdr/config.toml`. There was no reason to reinvent that.

#### How a column moves without closing

Each editor pane runs a loop around a *slot file* holding the section it is
showing: it writes the section, runs the editor, and reads the slot back when
the editor exits. Unchanged means you quit, and the loop ends, and the shell
exits, and herdr reaps the pane. Changed means the menu re-pointed the pane
while you were inside it, and the next turn opens what it now says.

That gives the context pane one gesture for two jobs. To **move** a section,
write the slot and ask the editor to save and quit. To **close** a column, ask
the editor to save and quit and leave the slot alone. Same keystrokes; the only
difference is whether anything was written first.

The slot is written at the *top* of each turn and read at the bottom, because
the menu writes it while the editor is running — any other ordering never sees
the change. And an empty read ends the loop rather than continuing it: the file
can go missing, and an empty section name would become `wsp edit <id> --`,
which parses as no section at all and puts the whole body in front of someone
who asked for one part of it.

**Nothing in the pane closes the tab.** It used to: a marker file counted
editors that had finished, and the second one took the tab down. That hard-codes
two panes into a shell fragment, and the moment the column count could change,
going from three columns to two counted as an editor finishing and closed the
tab underneath you. The context pane watches its editor siblings instead and
closes the tab when the count reaches zero — which does not care how many
columns there are. It arms only once editors have actually been seen, so a
context pane that paints before they register does not close the tab on the way
up, and it wants two consecutive empty readings, so a momentary gap in what
herdr reports is not mistaken for the end.

`W` in the context pane saves and closes every editor at once. It sends two
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
`overview.md`, `details.md` or `decisions.md` — so the editor's own status line
says which section you are in, without the editor having to cooperate. The label
is also how `W` and `q` know which panes are editors, and the test is the
section vocabulary rather than a pair of names: a pane holding `decisions` is as
much an editor as one holding `overview`, and naming them literally is how `q`
would have walked past one and closed the tab around it.

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
| `f` | idle agent | send it to find its own work |
| | | asks which project, and remembers, if it stands nowhere |
| `O` | task, project | open a herdr workspace for it, and claim it |
| `S` | task, project | the same, with an agent started in it and told |
| `X` | task, project | remove, after a `y`/`n` |

Nothing is reimplemented here: a key builds an argv and the panel runs its own
binary, so the event log, the hooks and the git commit all happen because it is
the same path a person at a shell takes. `wsp rename`, `wsp rm` and
`wsp project rm` exist because the panel needed them — `edit` opens `$EDITOR`,
which is no use to something already drawing on the screen.

`O` and `S` are both `wsp spawn` — see [Spawning](#spawning) for what it does
and why it is a CLI verb rather than a key. Two keys rather than one that asks,
because only one of them is expensive: `O` is a place to work and `S` is a
colleague with a model and a context window behind it, and a `y`/`n` between the
key and the thing would put that question in front of the cheap one every time.

`S` is the one key that does not answer on the next frame. Starting an agent is
seconds — a shell, a boot, a readiness handshake — so it runs off the event loop
and the footer says `starting an agent on …` while it happens. The panel goes on
drawing throughout; the outcome replaces the line when it lands.

Typing, picking and confirming are modes, not widgets: navigation and folding
keep working inside a pick, so you hunt for a destination by reading the tree.
`↵` takes the row it lands on, and on a row the pick cannot take but *can*
open — a folded project, a `⋯` tail — it opens it instead of refusing, because
those are the rows standing between the hunt and what it is looking for. A pick
started from the agents view puts the tree back first: `w` shows the panes and
nothing else, and it is the one view with no work in it to point at.

`c` and `f` are the two answers to one question, and the difference is who
picks the work. Both end in the same place: a sentence typed into the agent's
pane. A claim is a fact in the store and nothing at all in the pane it names —
an idle agent goes on sitting at its prompt until somebody types in it — so
`c` carries "you have been claimed onto `<id>`, run `wsp brief`", and `f`
carries "run `wsp next -p <proj>`, claim what it names, do it".

The project in that sentence comes from the same chain the agent's own
`wsp where` would use — pin, then mandate, then cwd, then workspace label — so
the panel can never send a pane to work somewhere it would disagree it is.
Note that this is *not* the branch of the tree the row is drawn under: a pane
mandated to `data` while standing in the `wsp` checkout is drawn where it
stands and told what it is for.

Most panes resolve to nothing, and that is not the odd case — herdr reports
where a pane's *shell* started, which for every agent launched from `~/claude`
is the directory above every checkout in it. So `f` on a pane it cannot place
turns the tree into the picker and asks which project it works, and the answer
becomes a **mandate** rather than being spent on the one keystroke: picking a
project for an idle agent *is* standing direction — "work here without asking"
is the whole content of the gesture — so the next `f` on that pane goes
straight out, and `wsp brief` inside it now says what it is for. Only a project
answers; a task or the inbox would scope `wsp next` to everything, which is not
what was asked. `wsp mandate --clear` in the pane undoes it.

`wsp next` answers for the *caller*, which is what makes this loop safe to
close. It offers `doing` and `todo` and nothing else. `review` is deliberately
absent, and its absence is the whole point: `Status::rank` puts review ahead of
both, so it did not merely appear in the list, it won — the first thing `f`
ever did was hand an agent back the task it had just finished and given to a
person. `blocked` is absent for the neighbouring reason: a decision is owed,
and that is not an agent's to make. Nor does `next` name work another live
agent is holding, because `claim` refuses exactly that — the two share one
definition of "somebody else has this" — and three idle agents set going at
once would otherwise all be handed the same task and all three bounce. Work in
the *caller's* own hand still counts: a `doing` task this pane already holds is
precisely this pane's next piece of work. When nothing is left, `next` says
which of the two reasons emptied the list, because "nothing actionable" on its
own reads as an empty backlog and usually means a busy one.

Three panes are never typed into. A shell would run the sentence as a command;
a working agent's prompt may not be a prompt at all, and a claim onto one still
lands, it just goes untold; an agent already holding a task is refused and
pointed at `v`. The sentence goes as two writes with a pause between, the same
bargain the editor panes make — a TUI that reads a burst of input as a paste
swallows the return on the end of it, and the instruction then sits in the
prompt unsent, which looks exactly like an agent that read it and ignored it.

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

A branch with no open work folds itself away until `A` asks for it. A project
holding *nothing at all* is the exception and always shows: there is no work
behind that row to go and look at, so hiding it tidies nothing — it takes the
project itself out of the panel, and that row is what `a`, `X`, `O` and `S` are
pressed on. So retiring a project's last task leaves the project where it was,
and a project that has never held a task can still be given one from here.

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

*Which* end of that list the cursor is on is part of where it is, and is kept
with it. Every row survives a rebuild by what it *is* rather than by the slot
it was in, because the tree re-sorts under the cursor constantly — but a pane
is deliberately drawn twice, under the task it claimed and again down here, and
the first row matching is always the one in the tree. Scrolling down off the end
of the tree therefore used to land in the dock and be dragged straight back up
to wherever that agent's work happened to sit, four times a second, for as long
as the cursor stayed there.

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

`wsp edit --decisions` exists anyway, and the two are not in conflict once the
rule is stated precisely. What append-only protects is that a decision cannot be
**quietly** rewritten — not that the text is immutable, which no file on disk
has ever been. A typo is worth fixing. A change nobody can see afterwards is
not. So the edit is allowed and it leaves a mark: editing the section by hand
appends `decisions edited by hand` to the log, and `wsp decide` does not, which
is how you tell the two apart a month later. Without that the section would have
been reachable from `E` and from the CLI with nothing recording that anyone had
been there — which is the failure the rule was written against, arriving by a
door the rule did not mention.

A title is not prose and is not edited here: `wsp rename <id> "new title"` is
the verb, and it writes `renamed from "…"` into the log on its way past. That
record is the point. A task that quietly became about something else, with no
sign in the file that it ever meant anything different, is how a backlog drifts
away from the plan it is supposed to be — and the id is what commits and other
tasks refer to, so a rename has to be the cheap operation rather than
delete-and-recreate. From the panel it is `e`, which runs exactly this.

```sh
wsp edit <id>                    # every prose section, headings included
wsp edit <id> --overview         # just that prose
wsp edit <id> --details
wsp edit <id> --decisions
wsp edit <id> --raw              # the whole file, when the frontmatter is wrong
wsp project edit <id> [--overview|--details|--decisions|--raw]

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

Projects carry the same sections and take the same editors — `roots`,
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
back in canonical order, headings you invented are kept, and if you delete every
heading and type anyway your text is filed under Overview rather than dropped.

Which sections exist is answered once, by `model::PROSE` — `SECTIONS` without
`Log`. It is one list because it was briefly three, and the third went stale:
`edit_prose` still named `Overview` and `Details` after `## Decisions` shipped,
so `wsp edit <id> --decisions` was not a section flag at all. It read as *no
section given*, took the combined-buffer path, found no headings in the incoming
text, and wrote the lot over `Overview` — reporting success. The flag a person
reaches for first, the moment the section is documented, silently destroyed the
prose it was aimed at.

Two rules came out of that, and both are about refusing to guess:

- **A flag `edit` does not know is refused**, rather than read as the absence of
  a flag. Guessing is what cost the prose, and a typo should cost nothing.
- **Presence decides what is written back, not content.** A heading still on
  screen with nothing under it means *clear this section*. A heading the buffer
  never carried means *this edit was not about that section* — leave what is
  stored alone. Collapsing the two is how a save wipes a section nobody touched,
  which is reachable from `--from` and stdin, which is to say from an agent.

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

A claim also renames the workspace and the pane after the task. herdr has no
name of its own for a workspace nobody named — it answers with the agent
standing in it, or the folder leaf — so three agents in one tree all read as
`claude`, which is the one thing about them you already knew. It renames over a
name you typed, too, and prints what it overwrote:

```
▸ t-260815-041  Agents should rename as they pick up new tasks
  bound to w0:p3
  named w0 · was wsp
```

That last line is the undo: `herdr workspace rename w0 wsp`.

A claim is the only moment it happens, so a pane that took its work up before
any of this existed keeps whatever herdr called it. `wsp reconcile` backfills —
it names every bound pane and its workspace after the task it holds, and picks
up any claim whose rename was dropped on a slow socket. The daemon runs it at
startup, so a herdr restart heals the lot.

Not in `sync`. That runs every tick, and a name reasserted every tick is a name
you cannot change by hand; here a name you type survives until the next
reconcile.

```sh
wsp say "reading the claim guard"   # the pane says where you have got to
wsp say --clear                     # back to the task's own name
```

The pane takes the sentence and the workspace keeps the task — a workspace
answers *what is this work*, a pane answers *what is happening in there right
now*. Putting both on the workspace would lose the name of the work every time
somebody started a build. A claim resets the pane to the task title, so there
is always a way home.

One sentence an agent does not have to remember to say: while it is looking for
work, `wsp next` and `wsp brief` say it for it — `looking for work in render`,
or `nothing actionable in render` when it asked and there was none. The claim is
what makes an agent visible from outside, and everything before it — reading a
backlog, writing the overview the task arrived without, deciding — used to leave
the pane wearing whatever it was called last, which from herdr is
indistinguishable from an agent that read its instruction and did nothing. State
first and project second, because the sidebar cuts the right-hand end.

Only a pane running an agent, and only one holding no task: a person asking
`wsp next` from a shell is asking a question, not reporting a state, and a bound
pane asking it is peeking past the thing it is in the middle of — `next` keeps
that pane's own `doing` task in the running, so the answer is usually the task
it already has. Nothing else needs a hook: `f` refuses on an agent that still
holds a task, so an agent sent looking is unbound by the time it asks.

The panel names a pane row by that label first, then its terminal title, then
the workspace. The label is the only one of the three anybody maintains: an
agent's terminal title is its opening prompt frozen, so a pane three tasks on
still announced the first thing it was asked — which is worse than a blank,
being a specific and confident answer that is wrong. So an agent row under a
task now reads as progress: the task above, `wsp say` beneath it.

The standing rules an agent is briefed with live in `~/wsp/agents.md`, not in
this binary — the user's to write, versioned with the tasks they talk about.
`wsp brief` prints them.

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

### Looking at a pane

```sh
wsp peek                 # the panel in this workspace
wsp peek view            # the detail pane beside it
wsp peek 042             # whichever pane holds that task
wsp peek --lines 40 --source recent
```

What is actually on a pane, as text. Not a screenshot: a terminal's contents
are already characters, and an image would need reading back out to answer
questions the characters answer exactly.

It exists because every interface bug on 2026-08-15 cost a round trip through
somebody's eyes. A key that did nothing because the binary was stale. A cursor
scrolled off the pane. A closed editor column still showing a shell prompt —
which was the bug, sat on screen for an hour, while the agent that wrote the
code read the code instead. The loop was guess, build for five minutes, install
into twenty-two panes, ask, wait. `peek` closes it: after installing a change to
anything a pane draws, look at the pane.

herdr could always answer this — `pane.read` has been there all along. What was
missing was a way to *name* the pane you mean. Nobody reaches for a capability
under pressure if reaching for it starts with listing every pane on the machine
and picking through the JSON, so the target resolves the way everything else in
wsp resolves: by what you call the thing. No argument is the panel, because that
is the question nine times in ten.

Two things it does not do. It shows what is on the pane *now*, not what happened
when a key was pressed — `herdr pane wait-output` is the tool for that, and it
is a different one. And it cannot press the key: a person still makes the
gesture, and what changes is that the agent can see the result rather than be
told it.

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

### Wiring it to Claude Code

```sh
chmod +x ~/claude/wsp/claude-code/wsp-session.sh
# then merge ~/claude/wsp/claude-code/settings.snippet.json
# into ~/.claude/settings.json — by hand, two keys
```

Two keys, and the snippet is a snippet rather than a file to copy over the top:
`settings.json` already holds your model, your theme, your plugins and herdr's
own `SessionStart` entry, and replacing it would take those with it. The wsp
hook goes *beside* herdr's in the same array — herdr's own file says in its
header that it is overwritten on every update, and it is right to say so.

`SessionStart` runs `wsp brief` and a session's context opens with it. That is
the whole integration: 38 ms, once per session, and an agent starts knowing its
project, the task it holds, what is already settled, what to take next and who
else is standing in the tree. A subagent is skipped — it shares its parent's
pane and therefore its parent's claim, and needs neither.

**`SessionEnd` is deliberately not wired.** A claim outlives the pane that made
it, which is the design; and `/clear` ends a session without ending the work.
Releasing there would drop a claim mid-task every time you cleared the screen,
and would take away the one signal `wsp wip` exists to give — a task underway
with nobody on it.

The **permissions** half matters as much as the hook. Create, claim, note,
decide and move through the workflow are pre-allowed, because a store an agent
has to ask permission to write is a store that stays empty. `done`, `rm`, `mv`,
`project rm` and `archive` are denied outright: finishing work and retiring it
are yours, and a deny rule states that better than a paragraph does — an agent
that finishes to `review` because the rule says so is one prompt away from
finishing to `done` when the rule is forgotten.

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

Which is the argument for keeping it short. The commit procedure was two thirds
of it, so every session read fifty lines of git ritual on the way in, before
knowing whether it would stage anything at all — and most never do. It lives in
`~/wsp/committing.md` now, printed on request by `wsp commit-help`, with one
line in `agents.md` pointing there. Same store and the same reasons; the
difference is that a procedure is read at the moment it is used, and a trigger
has to be read before it is.

```sh
wsp commit-help      # ~/wsp/committing.md, when you are about to stage
```

### Where an agent stops

```sh
wsp review <id>      # an agent's last word on a piece of work
wsp done <id>        # yours
```

`review` is the agent's terminal verb. It is not a request for a code review —
it means *I have finished and it is yours now*, which is a different claim from
`done` and the only one an agent is in a position to make. An agent that closes
its own work has graded its own homework, and the one thing it cannot know is
whether the thing you asked for is the thing it built.

Nothing enforces this and nothing should: `wsp done` works from anywhere, and a
rule that has to be policed by a permission is a rule nobody believes in. What
makes it hold is that it is stated in `agents.md`, where every session reads it,
and that stopping is *visible* — `wsp wip` grows a `REVIEW` block beside
`BLOCKED`, and the panel footer counts it beside the blocked count. Work parked
where you cannot see it is the failure this guards against, not disobedience.

`R` in the panel narrows the tree to exactly that work, and is the answer to
reviewing several things in a sitting. It is a filter rather than a second
pane: the panel already has the cursor, the verbs and the detail pane, so `d`
closes a task, `o` sends it back, `↵` opens it — every key goes on meaning what
it means, and the only thing that changed is which rows are there to aim them
at. Project rows stay so each item is placed, and their right-hand counts
follow the filter, because a project reading `5 ▸3 ■1` beside one visible row
is the tree describing a tree that is not there. The pane groups go, since a
terminal is not work waiting on you. The footer says the filter is up, because
one left on silently reads as an empty backlog — and `R` again is the whole
tree.

(`r` was taken: it syncs. Uppercase is what the panel already means by a
view-level key — `A`, `E`, `O`, `X` — and lowercase stays the verbs.)

The seam for telling you is already there. Every status change fires
`~/wsp/hooks/on-task-review` with the event JSON on stdin, so a notification is
an executable file rather than a feature request:

```sh
#!/bin/sh
# ~/wsp/hooks/on-task-review
exec terminal-notifier -title "wsp: ready to review" -message "$(jq -r .data.title)"
```

The payload is `{kind, ts, data}`, so the task's fields are under `.data` —
which is worth saying, because the obvious `jq -r .title` returns null and a
notification with an empty body looks like a broken tool rather than a wrong
path. Failures are ignored on purpose: a broken hook must not be able to stop a
task being finished.

## Sub-tasks

```sh
wsp add "Wire the daemon to the socket" --parent 005
wsp mv 007 --parent 005      # …or file it under 005 after the fact
wsp mv 007 --parent none     # back out to the top level
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
- **A task cannot be moved beneath its own descendant.** `add` cannot reach
  this, because a task being created has no children yet; `mv --parent` is the
  only verb that can, so the check is its own to make. The refusal names both
  ids. A cycle is worth refusing rather than reporting because it resolves at
  every step: nothing downstream notices, `nest` draws the loop flat, and the
  tree quietly hangs a row beneath itself.
- **Filtering never drops a child.** A task whose parent is not in the list is
  drawn at the top level rather than hidden; a row indented under nothing is
  just a row that looks broken.

Re-parenting arrived after sub-tasks did, which is why the tree was fixed at
creation for a while: the only way to regroup a backlog was to file the pieces
again under new ids, or to reach into `~/wsp` and edit the frontmatter — the one
thing the standing rules tell an agent not to do.

`mv` is where it landed, because "which project is this in" and "what is this
under" are the same question asked at two scales, and answering them with two
verbs is how they drift apart. The command keeps the first rule above by making
the project *follow* the parent:

```sh
wsp mv 007 --parent 005      # 007 lands in 005's project, whatever it was
wsp mv 007 --parent 005 -p x # refused unless x is already 005's project
wsp mv 007 -p x              # refused while 007 sits under a parent elsewhere
```

The last of those names `--parent none` in its refusal, because detaching is
usually what was meant, and a rule that does not say what to do instead is just
a wall.

**The move carries the sub-tree.** Re-parenting a task across projects takes
everything beneath it along, and each carried task says so in its own log. This
is the part that is easy to leave out and hard to notice missing: without it the
invariant survives exactly at the task that moved and breaks one level below it,
where nothing is looking. The carried writes land before the move itself, so the
single commit `mv` makes holds the whole thing — a sub-tree stranded in the old
project by a half-applied move is precisely the state the rule exists to
prevent. `counts_under` and the carry walk the tree through one shared function
for the same reason `tally` is shared: two walks are two chances to disagree
about what "beneath" means.

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
> claim > mandate > cwd > label**: a pin says what a workspace *is*, and a
binding and a claim are the work actually in hand, but a mandate beats the
directory a shell happens to be sitting in. It is deliberately not consulted
when the panel places a pane in the tree — where a pane is standing is a fact
about the pane, and standing direction says nothing about it.

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

## Spawning

Handing work to an agent that already exists is `c`. Getting one where there is
nobody at all is `spawn`.

```sh
wsp spawn t-260815-033             # a workspace, rooted where the work lives
wsp spawn t-260815-033 --agent     # …with an agent started in it and told
wsp spawn -p wsp --agent           # a project, no task, nothing to tell it
wsp spawn 033 --agent --kind codex # any agent kind herdr knows
wsp spawn 033 --agent --no-focus   # do not drag the screen over to it
```

It is a CLI verb before it is a key. The gesture used to exist only as `O` in
the panel, which meant a script could not do it and neither could an agent —
and an agent that can open a workspace for the sub-task it just filed is the
difference between decomposing work and doing all of it yourself. The panel's
`O` and `S` run this; there is no second copy of it behind the keys.

**Workspace, claim, agent, sentence, in that order.** The claim has to land
before the agent starts, because a Claude Code session runs `wsp brief` from
its `SessionStart` hook and reads the claim on the way in — started first, it
would open knowing nothing, and the sentence would be the only thing it ever
heard about the work. The sentence is the same one `c` types into an agent that
was already running, defined once, so an agent's work order does not depend on
which door it came through.

The claim is the one that can refuse — work that is done, work that is blocked,
work a live agent is holding — and each refusal is a reason not to put an agent
here: a spawn onto a blocked task is exactly what that guard exists to stop. The
workspace is left standing either way, because it is a terminal in the right
tree, which is what you would have opened by hand.

The workspace is rooted at the project's root, labelled after the work, and
carries `WSP_PROJECT` and `WSP_TASK` in its environment — so every pane inside
it knows what it is for instead of the cwd having to imply it. herdr does not
persist env across a restart, which is why the durable record is the claim; the
env is exact for the life of the session. A root is **inherited**: `wsp/render`
and `wsp/data` are two halves of one checkout and neither has a `roots` of its
own, and reading only a project's own roots put the agent wherever the caller
happened to be standing.

### What "started" means, and what it does not

`agent.start` does not wait for the agent. It answers immediately with
`launch_pending: true` — it has typed `claude` at the shell and nothing more —
and the wait herdr's own CLI does afterwards is client-side. Three things about
that window cost an afternoon between them, and all three are load-bearing:

- **A brand-new workspace has no shell yet.** `agent.start` ten milliseconds
  after `workspace.create` is refused with `agent_pane_busy`, "not an available
  shell". So the launch retries while that is the refusal, and gives up on any
  other.
- **`idle` does not mean ready.** herdr reports `agent_status: idle` while the
  agent is still starting, and `agent.prompt` refuses in that window with
  `agent_not_ready`. Waiting for `idle` therefore returned in half a second,
  every time, and the work order went into a pane still drawing its banner.
  `interactive_ready` is the field that means what `idle` looks like it means.
- **A shell that is not quite at a prompt eats what is typed at it.** The
  observed failure was ` mclaude`, `command not found`, and a minute of waiting
  for an agent that was never going to exist. So `ctrl-u` and one retype — but
  only while herdr can see no agent in the pane at all, because typing `claude`
  at a Claude Code that is merely still booting leaves the word in its input box
  for somebody to find later.

The sentence goes through `agent.prompt` rather than being typed into the pane,
which is what `c` still does: `c` speaks to agents herdr did not start and may
not consider ready, and pays for it with two writes and a sleep between them.
Here the readiness is established, so there is nothing to guess at.

## Two agents in one tree

Two agents worked this repository at once on 2026-08-15. Six times, one of them
took or removed work belonging to the other. None of the six was carelessness,
and each got past the defence built after the last one — which is the useful
part, because the defences were individually correct and collectively assumed
something that was not true: that a checkout belongs to whoever is looking at
it.

A shared checkout has four pieces of shared mutable state, and we found them in
this order, each by being bitten:

| Shared | How it bites |
|---|---|
| the working tree | your edit lands in their commit, or a `git stash` reverts their files under them |
| `.git/index` | `git add` writes to one index for everybody; whoever commits takes it all |
| `target/` | concurrent builds serialise on one lock, and neither build is attributable |
| `~/.local/bin/wsp` | whoever installs last decides what every running pane *and the daemon* is executing |

### What each check catches, and what it does not

This is the half worth writing down. Every incident happened inside the blind
spot of the check we were relying on at the time.

| Check | Catches | Blind to |
|---|---|---|
| Explicit paths (`git add <file>`) | staging a file you never meant to name | anything already staged in the shared index by someone else |
| Reading `git diff --cached` | a hunk you did not write, if you read the **file list** and not just the hunks | nothing — but only if you actually read it; every swept hunk looked plausible in isolation |
| Isolated build in a worktree | what you wrongly left *out*, and what you took in that only compiles against uncommitted work | whose commit it is; a coherent tree under the wrong author's message compiles perfectly |
| A private `GIT_INDEX_FILE` | the other agent's staged work entering your commit | the working tree; two people still editing one file; **the shared index, which nobody is now reading** |
| `wsp doctor` | a shared index holding something older than HEAD, in any declared project root | a shared index holding something *newer* — staged work is indistinguishable from staged work |
| `cmp` build against installed binary | a stale or partial install | nothing, and it is the only reliable one |

### The index nobody looks at

The seventh incident is the shape of the fix for the second. Once every agent
commits through a private `GIT_INDEX_FILE`, nobody reads `.git/index` again —
and one `git read-tree` or `git reset` run without that variable set puts an
older tree in it, where it sits looking like nothing at all. It was 4,962 lines
behind HEAD in this repo for most of an afternoon. Nothing was lost, because
everyone in the tree was following the procedure and none of them touched it;
the loss was one plain `git commit` away, from an agent that skipped a step or a
person at a shell.

`wsp doctor` reads it now, for every root a project declares. The rule is not
"something is staged" — that is an agent halfway through committing, and it is
none of doctor's business. It is **lines the index would drop that HEAD has and
the files on disk still have**: per path, the index's deletions minus the
disk's. Staged work subtracts to zero, and stays zero while its author carries
on editing, because everything the index drops the disk drops too. Only an index
holding something older than both survives the subtraction. Doctor strips
`GIT_INDEX_FILE` before asking, or it would inspect the caller's private index —
the one that is fine — and pronounce the loaded one healthy.

`git read-tree HEAD`, with no `GIT_INDEX_FILE` set, puts it back. It writes the
index and touches no file in the working tree.

It is step 4 of `wsp commit-help` now, beside the commit that causes it: a
private-index commit leaves `.git/index` holding the tree from before it, so
*every* commit made the right way leaves the shared index a commit behind. The
check found that thirty seconds after being installed, in its own commit. Doctor
names the state; the step stops making it.

### Do not search a release binary for a string

It cost an hour today, twice, in both directions. A short `&'static str` that
goes through `.into()` is not stored as a literal at all under `opt-level = 2`
with `lto` — the copy is inlined, so the bytes end up as immediate operands in
the instruction stream, split on eight-byte boundaries. `no editors open` is
absent from the binary; `no edito` and `rs open` are each present exactly once.
The bytes are all there. They are simply not a contiguous run for anything to
find.

Format strings survive, because the formatting machinery needs a real address
for them — which is what makes the method so convincing and so useless: some of
what you look for is always there. The proof it seemed to offer was that a
function had been optimised away, and the function in question was the `E` key,
which had been opening edit tabs all afternoon.

Use `cmp` against a clean isolated build, or run a command whose output only
the new code can produce.

The procedure that follows from all of it is `wsp commit-help`, in the order you
actually do it. The short version: commit through your own index, read the file
list before the hunks, prove it in a worktree, and verify the artefact rather
than the intention.

### What none of it fixes

Every rule above reduces the chance of taking someone's work; none removes it,
because the tree itself is shared. `t-260815-022` — one working tree per agent —
is the structural answer, and it is deliberately parked: it breaks
`project_for_cwd`, costs a checkout and a build per agent, and isolates only the
code, leaving the store, the index of record and the installed binary shared.
The judgement was that the rules are cheaper until agents run unattended. The
day nobody is reading the staged diff, that judgement changes.

The other half is not a rule at all. Three of the six incidents cost nothing but
the time spent not knowing what had happened, and all three were resolved by one
agent telling the other what it had just done. `wsp overlap` exists to make that
possible before the fact; saying it out loud is what makes it work.

## Source map

| File | Responsibility |
|---|---|
| `src/main.rs` | argument parsing, dispatch, help |
| `src/store.rs` | atomic writes, `O_EXCL` id allocation, git, state, hooks |
| `src/fm.rs` | the small YAML-frontmatter subset |
| `src/model.rs` | `Project`, `Task`, status/priority vocabulary |
| `src/resolve.rs` | project resolution, tag inheritance, sub-tree walk, count rollup |
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
| `src/detail/render.rs` | a task or a project, in full, and the column menu |
| `src/detail/editors.rs` | the columns, the editors, and the slot they read |
| `src/detail/run.rs` | the detail pane itself |
| `src/cmd_brief.rs` | one call for a session-start hook: where, what, who else |
| `src/cmd_mandate.rs` | standing direction: what a workspace is for |
| `src/cmd_spawn.rs` | a workspace on a task, and an agent started in it |
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
  bindings from claims against whatever is currently open. A *workspace* being
  closed is a decision rather than an accident, and `wsp reconcile --reap` is
  what ends the claims it leaves behind — asked for, never automatic, because a
  daemon starting while herdr is still restoring a session would read a
  half-built world as a mass closure. Herdr hands workspace ids out again, so a
  claim naming a closed one is not merely stale: it is waiting to attach itself
  to whatever takes the id next. The daemon does it
  before its first sync, which is when herdr has just restored everything under
  new pane ids. One pane takes one claim there too: two claims naming the same
  workspace used to land on the same pane, and since claims are walked in id
  order the agent came back bound to the *older* task — the one it had left.
- **cwd is not identity.** Five workspaces share `~/git/Easter` and eleven share
  `~/claude/vst`. Resolution order is pin → binding → claim → cwd → workspace
  label, so `wsp pin <project>` is the override when a directory is ambiguous.
  The claim step is what usually answers first: a binding says which task a
  *pane* holds and a claim says which task a *workspace* holds, and every pane
  in a claimed workspace is placed by it. Without that step, twelve of the
  twenty-one panes here piled under `vst` and the projects their workspaces were
  named after — `trance`, `ein`, `verb`, `jolt` — showed no panes at all, while
  the store had said where each of them belonged since the day `adopt` captured
  them. A claim from another host, or on work that is finished, is not work in
  hand and places nothing; of what is left the most recent wins.
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
