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
install -m 755 target/release/wsp ~/.local/bin/wsp   # the first one; after that, `wsp install`
wsp init                       # creates ~/wsp (git) and ~/.local/state/wsp
herdr plugin link "$PWD/herdr-plugin"
```

The plugin's `[[startup]]` launches `wsp daemon` on the next herdr start. To run
it in an already-live session:

```sh
nohup wsp daemon > ~/.local/state/wsp/daemon.log 2>&1 &
```

Sidebar rows go in `~/.config/herdr/config.toml` — see `[ui.sidebar.spaces]` and
`[ui.sidebar.agents]` there; `$proj`, `$todo`, `$doing`, `$blocked`, `$task` and
`$scope` are published by the daemon.

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

Every command commits the files it wrote, and only those — `wsp note` commits
one task, an archive sweep commits the set it moved, `wsp init` is the one
command whose subject is the whole store. It used to be `git add -A`, which
meant whoever ran a command next took everything every other agent had written
and landed it under their own message. Two things follow. A file you edit in
the store by hand — `agents.md`, a hook — is never committed by wsp, so commit
it yourself; `wsp doctor` names what is sitting there uncommitted. And a
command that fails to commit says so on stderr rather than leaving it for the
next one to sweep up, because now nothing will.

## Everyday use

```sh
wsp add "Port the reverb fix" -p trance --prio high
wsp ls                      # open tasks for the project you're standing in
wsp inbox                   # tasks with no project
wsp start 003               # ids accept a bare suffix or a title substring
wsp block 003 "waiting on the tuning table decision"
wsp prio 003 high           # what comes first inside its project
wsp done 003
wsp tree                    # hierarchy with rolled-up counts
wsp wip                     # every agent, its task, and who needs you
wsp where                   # what project am I in, and why
wsp overlap                 # who else is standing in this tree
wsp spawn 003 --agent       # open a workspace on it and put an agent in it
wsp despawn 003             # end that agent and release the claim
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
it changes, so `wsp install` reaches every long-lived process within a tick.
`exec` rather than a respawn: the pane, its pty and its place in the layout all
survive. Without it, twenty-two panes
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
closes it again), `esc` closes it, `Z` opens the whole tree in a tab of its own
and closes it again, `E` pops the row's file out into an editor tab, `1`-`9`
jump straight to an agent, `A` shows finished tasks, `R` narrows to what needs
reviewing, `w` shows the agents instead of the work, `W` puts the tree back with
the cursor on the task an agent is holding, `i` shows ids, `x` lowers a flag an
agent has raised, `r` syncs, `?` opens the key map.

### The strip, and the agents view

The top line carries one mark per running agent, and the same marks are the
first column of the agents view `w` opens. herdr reports two states — working,
or idle — and `idle` is an answer to a question nobody asked: an agent that has
stopped is waiting for *something*, and which something decides whether you
have to get up. The store holds the other half, so the two become four:

| Mark | What it is waiting for |
|---|---|
| `←` | stopped, holding a task that is still live — you are the blocker |
| `?` | stopped, on a task parked with a question written on it |
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
which is what the rest of them are. One click from a panel you are working in,
that is: the first click on a panel you are not brings the keyboard here and
does nothing else — see *How the tree scrolls*, below.

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

`W` is the way back out of it, standing on the work. The list answers who has
stopped; the next question is always what they stopped *on*, and until now the
only way to ask it was `w` again followed by hunting for the title by eye — in a
tree that had no idea which row you meant, and might not have been drawing it at
all. `W` on any agent row, in the list or in the section at the foot or under
the task it claimed, brings the tree back with the cursor already on that task,
and uncovers it if the tree was holding it out of sight: the projects above it
unfold, the cap comes off the list it is in, and a filter that would leave it
out — `A` for work that is finished, `R` for work that is not at review — goes
off, in that order, stopping at the first that is enough. Each of those is a
decision you made, so only the ones actually in the way are undone, and the two
filters say so in the footer as they go. An agent holding nothing is told so
rather than moved: that is a pane to give work to, which is `f` or `c`.

`i` puts each task's id in front of its title — the thing you type at a shell
beside the thing you read. Off by default: `t-260815-004` is thirteen columns of
a pane that is thirty wide and eleven of them are identical on every row, so
what shows is the suffix, which is exactly what `wsp start 004` resolves. When
another *open* task shares that suffix the date is what separates them and the
whole id appears instead; a finished task always shows in full, because a bare
suffix resolves against open tasks only and an id you cannot type is worse than
no id at all.

`F` docks the selected row's title under the tree, in full and wrapped, and
follows the cursor. A row is one line wide and a title is not: titles here run
to a median of sixty-four characters against the twenty-five a thirty-four
column row can draw, so the tree names most work by its opening clause. Reading
the rest meant `↵`, which opens a second pane and takes the cursor out of the
tree — a lot of ceremony for one sentence. With this up, scrolling *is* the
reading: every row you pass says what it is.

It keeps three lines whatever is selected, so the rows above do not step up and
down as the cursor moves between a short title and a long one, and grows to six
when the title needs them — a panel that cut the title would fail on exactly
the rows it exists for. Like the key map, it takes those rows out of the tree's
and gives them back when you press `F` again.

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
at. The tree simply has fewer rows to work with, and never lets the map push
the cursor off the bottom of them.

### How the tree scrolls

The view has a position of its own and keeps it. The cursor moves through it,
and the tree only gives ground when the cursor would otherwise walk off the
pane — two rows early, so there is always something beyond the cursor to read
towards, and one row at a time. Both ends clamp: on the first and last screens
the cursor rides up to the top and down to the foot, because there is nothing
further to show there.

The point of a view with a position is that most movement moves nothing else.
Reading down a pane costs no repaint of the rows you have already read, and
turning round costs nothing at all — what you are going back to has been on
screen the whole time. The tree used to be drawn from the cursor alone, held in
the middle of the pane, which meant every single row of travel scrolled
everything on screen.

The wheel moves the view rather than the cursor, three rows a notch, and moves
nothing else: it will carry the view clean off the selected row and leave it
selected. What is selected is something you decided, and going to look at
something else must not quietly change the row the next verb acts on. Nothing
is highlighted on the pane while you are looking away — press a cursor key and
the view comes back to it, and carries on from there.

A click moves the cursor and never the view: a pointer is owed the row staying
exactly where it was clicked, which is the one case that gets no lookahead.

A click on a panel nobody is working in does none of that, and brings the
keyboard here instead. The mouse reaches an unfocused pane — that is what makes
the panel worth pointing at — and the panel answers by taking focus, so a click
that acted as well would be two gestures at once. Point at the agent the cursor
is already on, which is the one you have been watching and is why the cursor is
there, and the click means `↵`: focus arrives here and leaves again for that
agent's terminal in the same movement, and you are somewhere you did not decide
to be by way of a pane you were only looking at. So the first click says where
you are working and the one after it means what it says. The wheel is not gated
that way — it cannot send you anywhere, and a pane you have to click into before
you can scroll it is worse at the one thing the panel is for.

The verbs are listed first because a pane too short for the whole map cuts from
the bottom, and movement is the half you can find by pressing an arrow and
watching. The footer says how many lines it could not fit.

### The whole tree, in a tab

`Z` opens the panel in a tab of its own — `wsp panel --full`, at the width of
the workspace — and `Z`, `q` or `esc` closes it again. It is a second panel and
it costs nothing to be one: the folds, the filters and the cursor are in the
store, so this and the sidebar are the same panel at two widths, and it opens on
the row you pressed `Z` from. Press `Z` in the sidebar while one is already open
and you go to it rather than getting a second.

Nothing is laid out differently. The tree stays one row to a line and every one
of those rows is the width of the pane, so the title that was twenty-five
characters and an ellipsis is a sentence, and there are as many rows as the
workspace is tall. That is the whole of what more room is for — the panel was
already the right shape, it was only ever thirty-four columns of it.

The one thing that does change is which rows there are: the six-task cap comes
off, so a project shows all its tasks rather than six and a `⋯ 4 more`. Six was
what one project could spend of a column that had to hold thirty projects, and a
pane this size has no such shortage. It follows the width rather than the key —
a pane is a page at 96 columns, which is about where a row stops abbreviating —
so dragging a sidebar wider by hand gets the same tree.

This was `pane.zoom` first, and that is worth writing down because it looked
right: zoom the pane the panel is already in, the pty resizes underneath, one
process and no tab to close. It cost a panel on the first afternoon — `Z`, a
switch to another agent, and the pane never came back.

A zoom is not a bigger pane. It is a display mode over the whole tab, set by one
pane and outliving it: measured against the live server, it survives a switch to
another workspace and back, and `pane.focus` will move the keyboard onto a pane
the zoom is hiding, so what is on the screen and what the keys reach stop being
the same question. The panel is furniture — installed in every workspace,
thought about by nobody — and furniture must not hold a workspace in a mode it
cannot see the state of and whose only undo is a key inside itself. A tab hides
nothing, herdr's own switcher lists it, and closing it puts you back.

The fullscreen panel gets its own detail pane, in its own tab: `↵` splits it
underneath, the same way the sidebar does. That is why `inspect` splits off *our
own pane* rather than the pane labelled `wsp` — with two panels in a workspace,
the label finds whichever herdr lists first, and a detail pane in a tab you are
not looking at is a `↵` that appears to do nothing.

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
| | | …and the panel takes the keyboard, because that sentence names a key |
| `P` | anywhere | new project, child of the selected one |
| `s` `v` `d` `o` | task | start, review, done, reopen |
| `b` | task | block, asking why |
| `e` `n` | task | retitle, append a note |
| `e` | project | rename it — its **name**, not its slug |
| `t` | task | tags: a picker — `␣` flips one, `↵` applies them |
| `!` | task | priority: high, then low, then normal again |
| `E` | task, project | edit its prose full-screen in a tab |
| `m` | task | move — the tree becomes the picker |
| `m` | project | the same, one level up: move it under another project |
| `c` | task or agent | claim, either direction — and how an agent moves on |
| `C` | task | the same claim, onto whoever is spare — no picking |
| `f` | idle agent | send it to find its own work |
| | | asks which project, and remembers, if it stands nowhere |
| `O` | task, project | open a herdr workspace for it, and claim it |
| `S` | task, project | the same, with an agent started in it and told |
| `x` | flagged task | lower a hand an agent raised — either row |
| `↵` | flagged task | the card again: what was asked, and by whom |
| `X` | task, project | remove, after a `y`/`n` |

Nothing is reimplemented here: a key builds an argv and the panel runs its own
binary, so the event log, the hooks and the git commit all happen because it is
the same path a person at a shell takes. `wsp rename`, `wsp rm` and
`wsp project rm` exist because the panel needed them — `edit` opens `$EDITOR`,
which is no use to something already drawing on the screen.

`e` on a project changes its **name**, which is not the string the row is drawn
with. The tree is slugs — `strata-strategy`, not "Questions & Strategy" — because
a slug is short, unique, and the thing you type at a shell, and every project in
a real store has a longer name behind it than a thirty-four column pane can
hold. So the panel draws the id and `e` changes the other one, which is what
`wsp project show` and the detail pane lead with. It is the only rename a
project has: nothing in wsp moves a slug, because every task, pin and mandate
refers to a project by it.

`m` on a project moves it under another one, sub-tree and all — the same
gesture as `m` on a task, one level up, and the same tree for a picker. Only a
project answers: there is no row that means the top of the tree, so detaching
one is `wsp project set <id> parent=none` and stays a deliberate act at a
shell. Landing on itself, or on anything already beneath it, is refused — and
refused by the CLI rather than by the panel, because the rule needs the subtree
and the panel has no index to ask. That refusal is not a policy with a
`--force` behind it: every walk over the project tree already stops on a cycle,
so nothing hangs, but a loop has no root, and a branch nothing can reach from
the root disappears from `wsp tree`, from the panel and from every list, with
its files still on disk and the command reporting success.

`t` opens a **tag picker**, docked under the tree: every tag the store already
uses, the task's own first and the rest commonest-first, `␣` to flip a row and
`↵` to apply the lot.

A picker rather than a prompt, and the removal is why. `wsp tag <id> +dsp -ui`
is the right shape for a shell and the wrong one for a sidebar: it makes you
spell out every tag, and the one you most have to spell is the one you are
taking *off* — a name the panel is already holding and you are being asked to
remember. The vocabulary is nineteen words across this whole store, counting
projects, so it fits on screen; and picking from what is there is also what
stops `dsp` and `DSP` becoming two tags that read as one.

Nothing is written until `↵`. That is what makes toggling safe to explore, and
it is why the marks distinguish four states rather than two — `✓` carried, `+`
about to be added, `−` about to come off, `·` not carried — because a mode that
defers the write has to say what the write will be. Toggling something on and
off again leaves the diff empty, and an empty diff runs no command at all: no
log line, no event, no commit. `esc` walks away from the whole thing. The rows
never reorder while the picker is up, whatever you toggle, because a list that
shuffles under the cursor is how you take off the tag next to the one you meant.

Typing narrows the list, and doubles as how a tag nobody has used yet gets its
name: filter to something the vocabulary does not hold and the last row offers
to make it, lowercased. One line for both, because they are one gesture — you
type what you want and take whichever of the two you are given. `␣` is free to
mean toggle because a tag with a space in it is not a tag.

The whole thing applies as a single `wsp tag <id> -- +new -old`. The `--` is not
decoration: the argument parser reads any token beginning with `-` as a flag, so
`wsp tag <id> -ui` otherwise answers with the usage line — half the verb, and
the half you reach for more often.

**Inherited tags are shown, and cannot be toggled.** This is the part that
looked broken before it was: a task under `render` reads `tags rust herdr` in
`wsp show` and in the detail pane, and owns *neither* — `render` owns nothing
either, and both come from `wsp`, two levels up. A picker offering only the
task's own tags drew `rust` as **absent** on a task every other surface says is
tagged `rust`, so taking it off looked like it had failed rather than like it
was never this task's to take off. They now draw dim, with a `✓` because they
genuinely are on the task, and the project they come from on the right. `␣` on
one says where to go instead.

A tag can be both — the task's own *and* the project's, which is the common
case for anything under `wsp` carrying its own `rust`. Then the lender is shown
on that row too, so a `−` reads as "removing your copy, and the project puts it
straight back": the removal is real, it just is not the whole story, and the row
says so before you press `↵` rather than after.

Tasks only — a project's tags are a whole list set at once by
`wsp project set <id> tags=…`, which is a different gesture with a different way
of going wrong, and it is where an inherited tag actually comes off.

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

`C` is `c` with the hunt taken out: it claims the task under the cursor onto
whoever is spare, and everything downstream is the same — the same
`claim --pane`, the same `--force` behind a refusal, the same sentence on the
same emptied context. Only "which pane" is arrived at differently. The pick is
right when you have somebody in mind and it is three keys and a walk down the
dock when you have not, which is the commoner case by far: you are reading the
tree, a task should be moving, and the only question about *who* is whether
anybody at all is free.

Spare means what the `○` in the strip means — stopped, and holding no live work
— so the agent it chooses is one you can watch it choose. Between two of them it
prefers the one already pointed at the work's own project, because that is the
pane standing in the right checkout; failing that, any spare agent takes it,
since "in another tree" is a better answer than "nobody has it". It refuses on
one thing only, that nobody is free, and says which of the two reasons it is.
Everything else — finished work, blocked work, work in somebody else's hands —
is `claim`'s to refuse, and comes back as the same `y`/`n` the pick gets rather
than a second copy of those rules living in the panel.

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

A pane gets its own row, drawn with the same mark and the same colour the strip
and the agents view give it: `●` working, `←` stopped in front of you, `?`
parked on a question, `○` spare, `·` not saying, and `▫` a shell with no agent
in it — never started, as against an idle agent that stopped. A task keeps its
own status glyph even when claimed, because the pane is on the row beneath
rather than borrowing the task's.

`←` marks an idle agent on a task that is still `doing` — it has stopped and
you are the blocker; the header carries the count. `⋯ n more` is the tail past
the six-task cap, which counts top-level work only: a sub-task is never hidden
while its parent is on screen.

`▲` is a hand an agent has raised about that task — see
[The raised hand](#the-raised-hand). It sits to the left of everything else the
title is preceded by, because it is the loudest thing on the row and is read
down the column rather than found along it; `x` takes it off.

`!` and `↓` are priority: ahead of the rest of this project's work, and behind
it. `normal` has no mark, because nearly everything is normal and a column
reserved to say so is two of the thirty-four a sidebar has. See
[Priority](#priority) for what the order means.

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

Colour carries seven roles: **bold** is a project, plain is a claimed task,
muted is an unclaimed one, dim is structure and finished work, accent is live
work, warn is waiting on you, and query — violet — is a question somebody has
written down. The last two were one colour until the tree started drawing
agents the way the strip does: a nudge and an answer are different things to be
asked for, and a stopped agent sits a row below the task it stopped on, so one
orange for both left the pair unreadable exactly where they meet.

Note `▸` does double duty — a folded caret on the left of a project row, a
count of tasks in flight on the right.

### The raised hand

An agent cannot point. It can write a task up, claim it, park it with a
question on it — and all of that lands somewhere you have to go and look. What
it could not do was say *this row, now*, which is the difference between a
backlog read at the end of the day and a question that gets answered.

```sh
wsp flag <id> "this is next — can I take it?"
wsp flag <id>                # …or just: look at this, it exists
wsp flag                     # what is raised
wsp flag --clear <id>        # lower it
```

It arrives as a section pinned above the agents, in every panel, without
anybody pressing anything — a panel is already installed in every workspace and
they all read the same shared view, so nothing has to find a window or know
which screen is in front of you. The row draws the sentence, because the
sentence is the news: the task's own title is up in the tree with a `▲` on it,
and the pane that raised it is on the right. A flag with nothing written on it
draws the title instead, which is the whole of "look at this, it exists".

**The row is the deeplink.** It stands for the task it points at, so every verb
already aimed at a task works from it and none of them had to learn that flags
exist: `↵` opens it, `c` claims it, `s` starts it, `E` writes it up. The task's
own row carries the mark as well, because once you have read the ask the next
question is what the work sits beside, and only the tree answers that.

`x` lowers it, from either row. Deliberately not something `↵` does on the way
past: reading the ask is how you decide, and a flag that cleared itself the
moment you looked would be gone from every panel before you had decided
anything about it — including the decision to leave it up while you finish what
you are doing. `↵` on a flag row brings its card back.

**A raised hand is not on the background cadence.** A panel nobody is looking
at refetches every thirty seconds, which is right for news about work — nothing
is waiting on that pane to notice a status change — and wrong for a card, in
both directions: half a minute before an agent's question reaches you, and half
a minute of a card you have already answered still standing over the other
twenty-one panels with the keyboard in it. So the flag file's timestamp is read
on every tick, ahead of the gate. It is one `stat`, against the `readdir` of two
directories and the two socket calls a refetch costs — a hundred a second across
every panel on the machine, and cheaper than the status poll that already runs.

It is pinned in every view, `R` and `w` included. Those are filters over
*work*, and a hand raised is somebody asking for you rather than work waiting;
a section that went quiet under one would be a section you learn to distrust,
and the view you happened to leave the panel in would decide whether an agent
could reach you at all. Three are drawn and a `⋯` opens the rest, so a burst of
asks cannot take the whole dock and leave nothing of the census beneath it, and
the heading counts them all. On a pane too short for both, the flags are the
half that stays: the header strip carries every agent in every frame, and
nothing else on screen carries a raised hand. The footer carries the count too, for a pane too short to give
the section any rows.

#### The card

A row says a hand is up. It cannot say what was asked — thirty-four columns
against a paragraph and a question — and it does not *arrive*, it waits for you
to look down. So a raised flag opens a card over the tree:

```
 ┌──────────────────────────────┐
 │ ▲ the panel work, in order   │
 │                              │
 │ Items 1-4 are done and this  │
 │ one is the first that is …   │
 │                              │
 │ w4:p2 asks to take this on   │
 │ y hand it over · n no        │
 │ o open · esc later           │
 └──────────────────────────────┘
```

```sh
wsp flag <id> --title "the store will not parse" --body - --ask claim
```

Three optional parts, because the cheap version has to stay cheap: `wsp flag
<id>` on its own still works, and the card fills itself in from the task — the
task's title as the heading, and its Overview as the body. A title of its own
matters when the task's is the wrong sentence for the moment; a body carries
the paragraph a row cannot; and an ask is the one thing a keypress can settle.

It is the only thing on the panel drawn with a border. Everything else here is
furniture that is always there, and a rule is enough to separate that; this
arrived, it is in front of what you were reading, and it is going away again.
Inset by a column so the rows show at both edges, which is the difference
between a card lying on the tree and a pane that has replaced it. It never
covers the section at the foot: answering one ask must not hide the queue of
the others, and the footer says `1 of 3 raised` so a second is never a surprise.

**It holds the keyboard.** Every other mode here is entered by a key, so
whoever opened it knows it is open; this one lands in front of whatever you
were reading, which is exactly why `d` typed a quarter-second earlier must not
close a task and `y` must not reach the tree behind it. For the same reason it
only comes up over an idle panel — a half-typed title is a sentence somebody is
in the middle of, and the card waits for it rather than taking the keys meant
for it. And it is asked once: a panel that popped whatever was unread on every
frame would put the same card back up for ever, with `esc` doing nothing you
could see.

| Key | Means |
|---|---|
| `esc` | not now — the card goes, the hand stays raised |
| `x` | dealt with — the flag comes down everywhere |
| `o` | open the task in the detail pane, card still up |
| `y` `n` | answer the question, where there is one |

`↵` puts away a card that only wanted to be looked at, and is deliberately
*not* an answer to one that asked for something: a return pressed out of habit
must never hand a task over.

**The asks are a closed vocabulary** — `--ask claim`, and nothing else yet.
That is the whole of the security model here: the answer to a card is a
keystroke that runs a command, so an agent naming its own argv would be an
agent deciding what `y` does on somebody else's panel. It names a question the
panel already knows how to answer instead, and the panel decides what answering
it means.

`y` runs the same `wsp claim <id> --pane <asker>` that `c` builds from the
other direction — the agent picked the task and you picked the agent — and the
flag comes down because **`claim` lowers the flags on the task it claims**,
rather than because the panel ran a second command to tidy up after the first.
A hand raised about work now in somebody's hands is a question about the past,
however it got there.

`n` lowers it and types the refusal back into the agent's pane. A refusal is
worth saying: the agent asked and carried on with something else, and silence
is indistinguishable from an answer that never came. It goes in without a
`/clear` in front of it, unlike every other sentence the panel sends — those
hand over new work and want the last task's reasoning out of the way, and this
one is an answer to a question asked *during* work the agent is still holding.

**A flag is state, not store.** It sits beside the claims and the pins in
`~/.local/state/wsp`: no commit, nothing in the task file, nothing left in the
history of the work once it is answered. A raised hand is true while somebody
is at the machine and meaningless a week later. `wsp note` is the verb for the
half of it worth keeping — and the two are a pair, not a choice: flag it so it
is seen today, note it so it is read tomorrow.

Something louder is a hook away, the same seam `review` uses:

```sh
#!/bin/sh
# ~/wsp/hooks/on-task-flagged
exec terminal-notifier -title "wsp: $(jq -r .data.id)" -message "$(jq -r .data.said)"
```

The panel is the deeplink and this is the doorbell. Which is the honest split:
nothing here can raise a window on a machine it is not sitting at, and nothing
needs to — the surface you are already looking at is the one to reach.

### Storyboard

`wsp panel storyboard [--out page.html]` renders the panel offline: fixtures for
layout, and flows that push scripted keys through the same reducer the live
panel runs. No herdr, no store, no terminal — useful for arguing about the
design before building it. The legend above is generated from the same glyph
constants and `Style` values the rows draw with, so it cannot drift.

## What a task or project file holds

Frontmatter is a contract — `id`, `status`, `schema` — and every field in it has
a command that sets it correctly. The body is yours, and carries four sections —
five on a project:

| Section | For |
|---|---|
| `## Overview` | what the task is, written once, read to re-enter it |
| `## Details` | working material — criteria, links, whatever the work needs |
| `## Handbook` | *projects only* — what an arriving agent is told; see below |
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

`## Handbook` is where that line gets drawn explicitly, and it is a project
section because a task has no equivalent — `wsp edit <task> --handbook` is a
typo and is refused as one. It holds what somebody arriving on this project is
told: what the work is for, the conventions that are conventions of the *work*
rather than of the code, and — the part that keeps it short — **a pointer to the
repository's own documentation, naming what is in it**. It does not hold a map
of the tree. That belongs in the tree, versioned with the code, reviewed in the
same diff, and put in front of whoever changes it; the same content kept here
drifts the moment somebody refactors and nothing makes them look at it.

```sh
wsp project edit wsp --handbook -        # from stdin, like any other prose
wsp project show wsp                     # one line: how many, and how to read it
wsp project show wsp --handbook          # as written
```

Abridged by default in `show` for the reason the decisions block is: it is
written to be injected once at the top of a session, where the model re-reads it
for free, and `project show` is a command an agent runs several times a session.
Printing it into each of those is the multiplication the mode below exists to
remove.

It is inherited down the project chain the way tags and decisions are. `wsp`
says where the code's documentation lives and how to build here; `robustness`
adds what is true of `robustness`; neither has to repeat the other, and an agent
claimed onto `robustness` is handed both, root first.

Dated and append-only, like the log, and for the same reason: there is no `wsp
undecide`. A decision that turns out wrong is superseded by a later one saying
so, which is the honest record — what a reader three months on needs is the
reasoning that was live at the time, not a tidied conclusion. It sits above the
task list in `wsp project show`, because it is a constraint on what may be
picked up next and belongs in front of the list of things somebody might pick.

Append-only and never trimmed is also the one output here that grows without
bound, and it grows fastest on the projects people are actually working. `wsp`
took eighteen decisions in a day, at which point `project show` was printing
16KB — 3,437 of its 4,104 tokens were that block, three times the next most
expensive command a session can run, and the brief was pointing every agent at
it to read the four it had left out. So the block is an index by default: one
line each, the first sentence, which is the rule — the argument for it comes
after, and it is not what a reader scanning for *which* decision needs.

```sh
wsp project show wsp               # decisions abridged to the rule, ~600 tokens
wsp project show wsp --decisions   # as written, reasoning and all
```

Abridging is never silent — the block says how many entries it cut and what to
run — for the reason the rules cap has: prose that stops early reads exactly
like prose that ends there, and here the missing half is the reasoning somebody
is otherwise about to re-derive.

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

Every name wsp writes leads with the task's scope — `render/109 · reading the
claim guard` — because a collapsed sidebar is a rail a few columns wide, and the
right-hand end is what it cuts. Three agents working through one project all
wore titles that began the same way, and telling them apart meant widening the
sidebar and reading each one to the end. Ten columns answer it instead, and they
are the ten you would type to go and look: `wsp show 109`. The sentence keeps the
scope too — saying something should not cost a pane its place in the list — and
a pane holding no task has none to wear, which is itself the difference between
an agent on a piece of work and an agent between two. The same string goes out
as the `$scope` token, for a sidebar row that would rather carry it on its own.

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
wsp brief --session  # …and the work itself. The hook's call; see below
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

### The session payload

```sh
wsp brief --session
```

An agent that arrived holding only a task *title* went and fetched the rest.
Measured across the two agents spawned on 2026-08-16: nineteen `wsp show` calls,
14,450 tokens, rebuilding context the session that spawned it already had — and
because every request re-reads the whole context, arriving over requests 4–16
meant paying for it another ~86 times.

So the hook hands it over instead. `--session` adds, after the roster and before
the rules:

```
overview <the claimed task's own prose, as written>
settled  <what the task has already decided>
log      <the tail of its log — where direction dropped on it later arrives>
binds    <the parent's decisions: direction lands on a parent>
names    <the siblings its prose mentions by id, with title and status>
read     <the handbook, down the project chain, root first>
```

Measured on this repo, on a real task: plain `wsp brief` is ~700 tokens and the
payload is ~3,300 — ~1,150 of it the handbook, which is constant per project,
and the rest the task. Against 14,450 fetched late, that is roughly 4:1 in
favour, and it removes nineteen round-trips as well, each of which is itself a
full context re-read.

**The same arithmetic is why plain `wsp brief` must not grow.** It is run
constantly, mid-session, by every agent and by whoever is coordinating — so a
thousand tokens added to the default is a thousand tokens times every call in
every session on the machine. That is the whole reason this is a mode the hook
passes rather than a better default, and there is a test asserting that none of
the payload appears without it.

`names` answers *what is `t-260816-060`* with a title and a status and nothing
more. Answering it with the whole task would be the fetching this replaces, done
eagerly, for every id rather than the one that mattered.

The work order changes with it. `wsp spawn --agent` claims *before* it starts
the agent, so by the time the agent is told what it is holding, its hook has
already run with the claim in place — and the sentence stops asking for a brief
it has. The panel's `f` hands work to an agent that has been running since
before the claim existed, so that one is still asked to fetch, once, with
`--session`. The duplicate is gone by construction rather than by remembering.

### What it is *not* handed

```sh
wsp spawn <id> --agent          # trimmed
wsp spawn <id> --agent --full   # everything, for the spawn that needs it
```

The brief is not the expensive half. A spawned Claude Code session starts at
~37,000 tokens before it has done anything, of which `wsp brief --session` is
~3,300 — the rest is Claude Code's own system prompt, its tool schemas, and the
instruction prose every configured MCP server contributes. All of it is re-read
on every request.

So `spawn` starts the agent without three things, named in `cmd_spawn::TRIM`:

| dropped | why |
|---|---|
| the `Workflow` tool | the work order forbids it in as many words |
| the `Agent` tool | same — and sub-agents are what blew the budget in the first place |
| every MCP server, and its instructions | two measured agents made zero MCP calls between them |

Measured live on 2026-08-17, two `wsp spawn --agent` runs into a sandbox on the
same task, read back off the transcripts: **37,756 → 25,306, a third off every
request.** Worth stating against the estimate that prompted it, which hoped for
28.6K: the system prompt underneath the schemas is ~25K and no flag reaches it.
This is 44% of what was hoped for and the rest is not on offer.

It is a *denylist*, and deliberately not the allowlist the estimate called for.
`--tools` takes an allowlist, and the first attempt here used one —
`Bash,Read,Edit,Write,Glob,Grep,TodoWrite,Task`. Four of those eight do not
exist in this build; unknown names are ignored in silence, so the list measured
something other than what it said and quietly withheld tools nobody had
considered. An allowlist withholds everything nobody thought of, including
everything Claude Code gains after the line was written, and an agent that
silently lacks a tool does not report it — it works around it, expensively and
out of sight. That is the failure this is supposed to prevent, so the list names
what it takes and takes nothing else.

`Read`, `Edit`, `Write` and `Bash` are not on it and must not go on it. The
measurement that started this found agents doing all their reading through `sed`
and `head` at ~28K, which is the same failure one level down: remove the
affordance and the work still happens, just worse.

`--full` is the way back, and there has to be one, because a trim is a
capability change — the agent that needs the design MCP server to draw an
artefact is a spawn on this backlog rather than a hypothesis. The trim applies
to `--kind claude` only; these are Claude Code's flag spellings and handing them
to `codex` buys a workspace with a shell in it and no agent.

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
decide, flag and move through the workflow are pre-allowed, because a store an
agent has to ask permission to write is a store that stays empty — and `flag`
doubly so: an agent that has to ask before it can ask is one that never does. `done`, `rm`, `mv`,
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

## Priority

```sh
wsp add "Port the reverb fix" -p trance --prio high
wsp prio 003 high            # …or say so afterwards, which is the usual way
wsp prio 003 normal          # and take it back
```

Three levels — `high`, `normal`, `low` — and `normal` is what almost everything
is. The field is old; being able to *change* it is not. `--prio` on `add` used
to be the only way to set it, which put the decision at the one moment you know
least about the work and then left it there. Phases 3 and 4 of the strata plan
traded places, `high` stayed on the one that had become later, and every agent
asking `wsp next` was pointed at the wrong end of the plan by a field nobody
could move.

**It orders one project's tasks against each other, and nothing else.** There is
no global queue and no number to compete over: `!` on a task in `trance` says
what comes first *in trance*, and says nothing at all about anything in `wsp`.
The tree is what keeps that honest — two tasks in different projects are never
in the same list to be sorted.

**Under status, not over it.** `ls`, `brief`, `next` and the panel all sort by
status first and priority second, so raising something that has not been started
does not float it above work already in hand. Priority breaks ties among tasks
that are otherwise equally ready; it does not interrupt.

A change lands in the task's own log — `priority normal → high` — because a
backlog whose order moves with no record is one nobody can read a week later.
Setting the level a task already has does nothing at all: no log line, no
commit. From the panel it is `!`, which cycles `high` → `low` → `normal`, so
the same key both sets it and puts it back.

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

## The seat

```sh
wsp govern robustness    # this workspace coordinates robustness
wsp govern               # who is seated, and which seat answers for here
wsp govern --clear       # stand down
```

On 2026-08-17 one workspace ran twelve agents across the `robustness` backlog
for a night, and it worked. It worked by convention: the seat was an ordinary
claim on an ordinary task that happened to be the artefact it was writing, so
nothing in wsp could tell the agent *sequencing* the work from the agents doing
it. Two things went wrong all night, and they are what this verb removes.

**`wsp wip` drew the seat as an agent that needs you.** Idle process on a
`doing` task means a person has become the blocker — for a worker. A seat is
idle *between* the agents it is waiting on, which is most of the time, so the
loudest row on the panel was the one that never meant anything. Now it reads:

```
PROJECT   TASK                                    PANE     STATE
robustness  build a design artefact                w1:p6   idle     ▣ seat · robustness
wsp         the seam under the panel               w2:p1   idle     ← needs you
```

**A raised hand had nowhere to go.** `wsp flag` said *raised on every panel*,
because a person's screen was the only destination there was. With a seat, the
receipt names it, `wsp flag` marks whose each hand is, and `wsp flag --seat` is
the seat's own inbox:

```
▲ t-260816-094  reconcile erases every agent's sentence
  can I take this? · w4:p1 · 6m · ▣ yours · robustness
```

**Per hierarchy, like everything else that is inherited.** `wsp` has a seat,
`robustness` has its own, a sub-project may have one. A hand raised on a `data`
task asks `data`, then `robustness`, then `wsp`, and takes the first answer —
so there is no escalation step, only a walk that does not stop at a level with
nobody in it. **The chain always terminates, because the person is the governor
of last resort**: nothing above it means the flag is raised on every panel,
which is exactly what happens today.

That is also the whole answer to *what if there is no seat*, which is the normal
state. No governor anywhere is one missing file and one empty map, and every
output in this tree is byte for byte what it was — including `wsp brief`, which
draws the `seat` line only when there is a seat above the pane reading it. The
brief is re-read on every request of every session, so a line that is present
when nobody is coordinating is paid tens of thousands of times a night for a
fact that never changes.

**A coordination point, not an approval gate.** No verb consults a seat before
acting. A governor changes who is *expected to look* at a raised hand and how a
seat is *drawn*, and changes nothing about what any agent may do — a gate here
would put a round-trip in front of every agent for the benefit of none. The one
guard runs the other way: `wsp despawn` refuses to end a governing pane without
`--force`, because the seat is the one agent whose thread does not come back,
and that refusal costs the seat rather than anybody under it.

The record is keyed on the **project**, which is the one place this differs
from the pins, mandates and claims beside it. Those are facts about a workspace
and there can be many per project; there is at most one seat per project, and
every reader asks *who governs this?* rather than *what does this workspace
govern*. It is state rather than store for the same reason a claim is: the
hierarchy is committed and durable, and the agent sitting in it is neither.

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

### Saying less

Almost everything an agent knows about the work, it read out of this tool, and
that reading is charged to its context. 988 `wsp` calls across 221 sessions on
this machine came to roughly 202,600 tokens of output.

Most of that is content and stays. An `ls` row is twenty-one tokens of id,
status, priority and title with nothing in it to remove; `show` is the task's
own prose, which is the work in hand and the one place you want all of it;
`where`, `doctor`, `overlap` and `next` are already under 500 bytes. Three
things were not content, and each was dealt with where it was:

| | |
|---|---|
| `project show` | the decisions block, printed whole. 4,104 → 1,058 tokens, for every caller |
| `agents.md` | fifty lines of commit ritual (66058d9), then the case for each remaining rule. 434 → 305 tokens, in every session on the machine |
| the figma plugin | 3,545 tokens of system prompt in a Rust TUI that cannot reach any of it (ee9ae79) |

What is left is a block you have already read, printed again because the
command that carries it does not know that. `--terse` — or `WSP_TERSE=1`, set
once, for a whole session — leaves those out:

```sh
wsp brief --terse    # everything but the rules      613 → 319 tokens
wsp wip --terse      # the blocked count, not the list  259 → 140 tokens
```

Two commands, because those are the two that get re-read. The session hook
delivers the rules once and then every later `wsp brief` in that session pays
about three hundred tokens for text a few thousand tokens up its own context —
thirty-five of those in the sessions measured. `wip` is asked repeatedly to see
who is free, and the blocked list is the slowest-moving thing in it, because a
task is blocked on a person.

Neither block goes quietly: each leaves a line saying it is gone and what to
run. Prose that stops early reads exactly like prose that ends there, which is
the failure the rules cap is written against.

`project show` was tried as a third and dropped. Its `--terse` saved 266 bytes
on `wsp` and nothing at all on four other projects, none of which carry an
Overview or Details — a flag that does nothing on four things out of five is a
flag nobody believes. Its expensive half was the decisions block, and that is
fixed for everybody rather than for whoever remembers to ask.

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

`spawn` says `open`, `start`, then `tell`, and it says them to a *backend* —
`src/place.rs` is the port and `src/place_herdr.rs` is herdr behind it. Two
waits, and they are different questions: `start` returns when the agent exists,
and only a state of `idle` in the port's sense means it will listen. What has to
be retried to get there is the backend's business and is no longer written into
the command.

Which matters because `agent.start` does not wait for the agent. It answers
immediately with `launch_pending: true` — it has typed `claude` at the shell and
nothing more — and the wait herdr's own CLI does afterwards is client-side.
Three things about that window cost an afternoon between them, all three are
load-bearing, and all three now live in the adapter:

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

The sentence goes through the port's `tell` — `agent.prompt` under herdr — rather
than being typed into the pane, which is what `c` still does: `c` speaks to
agents herdr did not start and may not consider ready, and pays for it with two
writes and a sleep between them. Here the readiness is established, so there is
nothing to guess at.

### Ending it

```sh
wsp despawn 033                    # end the agent working it, and put the work down
wsp despawn --pane w26:p1          # …or name the seat, bound or not
```

What this replaces is two commands, one of which is not wsp: `wsp release --pane
w26:p1` and then `herdr workspace close w26`. **Stop first, release last** — the
reverse of the order it was done by hand, because the two failures are not
comparable: a seat that will not close keeps its claim and says so, while a claim
released over a live agent is handed straight to the next one. A seat that was
*already* gone counts as done, since an agent whose backend died under it is the
ordinary case for this verb. The argument in full, including why ending a seat
ends the claim rather than only the binding, is in `src/place.rs`; under herdr the
verb is `pane.close` on the seat, and `src/place_herdr.rs` records what was
measured to choose it over `workspace.close`.

## Machines

A second machine, driven from the one you are sitting at. Same projects, same
store, same panel — a bigger hand.

```sh
wsp machine add mb2 mac-mini       # a name, and an ssh Host alias
wsp machine ls                     # what exists, and whether it is answering
wsp machine show mb2               # ssh target, tunnel, last seen, why not
wsp spawn 033 --agent --on mb2     # open the work over there
```

The far machine is an **executor**, never a seat. Nobody sits at it. It holds
no store, no `~/.local/state/wsp` and no wsp binary — the seat is the one source
of truth, which is what makes there be no sync of `~/wsp`, no merge story for
two git repositories that both autocommit, and nothing on the far machine to
hot-patch when you ship a fix. `wsp` over there is a shim that runs this
machine's wsp back over the same connection.

### What is actually running

On the executor, three things, none of them ours:

- `herdr server`, headless, under launchd or systemd so it outlives the ssh
  session that started it and comes back after a reboot
- sshd
- the `wsp` shim, from `executor/wsp` in this repository

On the seat, inside the `wsp daemon` you are already running: one ssh per
machine, holding `-L <local.sock>:<remote herdr.sock>` open. That is the whole
of it. Nothing long-lived of ours runs on the executor at all.

### How one wsp talks to two herdrs

herdr has no idea another herdr exists. Its socket API is 89 methods and none of
them has a host in it: one server is one machine. Two facts make spanning them
cheap rather than a rewrite.

**The socket is forwarded, not proxied.** `ssh -L` (OpenSSH ≥ 6.7) puts the far
machine's herdr socket at a path here, and wsp's existing herdr client speaks to
it unmodified. There is no second protocol and no re-implementation of those 89
methods.

**The id is the routing key.** Nearly every herdr method is addressed by a
`pane_id`, a `workspace_id` or a `target`, so a remote pane is `w0:p3@mb2` and
`herdr::call` splits the `@machine` off to pick the socket — stripping it on the
way out, because the far herdr has never heard of it. Local ids stay bare, so
no existing call site, state file or claim changed. `@` and not `:`, because a
herdr pane id already contains a colon: `w0:p3` is one id, not two.

The calls that carry no id — `pane.list`, `workspace.list`, `agent.list`,
`events.subscribe` — fan out across every reachable machine and qualify what
comes back, so a pane is routable from the moment it enters wsp and everything
downstream is unchanged. `workspace.create` is the exception in the other
direction: nothing to route on, so `spawn` tells it, which is what `--on` is.

### Three states, and the middle one is the point

`wsp machine ls` says **connected**, **offline** with a last-seen, or **retired**.
The distinction that matters is between a machine that is answering with nothing
and a machine that is not answering at all.

`wsp reconcile --reap` ends a claim whose workspace is gone. A machine that is
merely unreachable reports no workspaces — which looks exactly like a machine
with nothing running on it. Reaped on that basis, one dropped link hands back
every task the executor is holding while its agents carry on working on tasks
that no longer know about them. So a machine has to have been *heard from*
before anything it holds is touched, and "unreachable" is a third state rather
than a synonym for empty. It is the one thing in this design that is worth
being paranoid about.

### The network is `~/.ssh/config`, and Tailscale under it

A machine in wsp is a name and a `Host` alias. wsp never parses ssh config,
never learns an address and has no idea a tailnet exists — that ignorance is
the seam, and it is why "make the machine reachable" is not a wsp feature.

Tailscale is what makes the address stable, chosen because the seat is a
laptop: its address was never going to hold still, and plain LAN ssh would have
broken the first time either machine changed network. MagicDNS gives each
machine a name that resolves from anywhere with no inbound firewall rule and no
port forward, and that name is one `HostName` line. Plain ssh over the tailnet
rather than Tailscale SSH — the latter replaces key auth with tailnet ACLs,
which is a second auth path to debug for no gain. Headscale is available on
identical terms if the coordination server ever becomes the objection.

Two costs worth knowing. `tailscaled` is now a thing that can be down, which
makes the unreachable-is-not-empty case above more likely to fire rather than
less. And we are tunnelling a chatty JSON-RPC socket that the panel polls, so a
connection that falls back to a DERP relay instead of a direct WireGuard path
will be *felt*: check `tailscale status` for `direct` before blaming wsp for a
slow panel.

### Making a machine an executor

On the far machine:

1. `tailscale up`, and confirm `tailscale status` shows a **direct** path.
2. Enable Remote Login (macOS) or sshd (Linux), and authorise the seat's key.
3. Install herdr and run `herdr server` headless under launchd or systemd.
4. Put `executor/wsp` on PATH ahead of anything else called `wsp`, and set
   `WSP_SEAT` and `WSP_MACHINE` for every shell an agent might run in — a
   `launchctl setenv` on macOS, or the profile the herdr server inherits.
5. Install Claude Code, authenticate it, and add the `SessionStart` hook from
   `claude-code/`. It needs no change: it finds the shim and the shim finds the
   seat.

On the seat:

6. A `Host` block in `~/.ssh/config` pointing at the MagicDNS name, with
   `ControlMaster auto` and `ControlPersist` so the connection is cheap to
   reuse.
7. `wsp machine add mb2 <that Host alias>`.

The daemon takes it from there: it dials, holds the tunnel up, retries with
backoff, and writes what it can see into `wsp machine ls`.

That is the whole install, and its being a list rather than a script is the
point — replication was the requirement, and anything longer would have meant
the design had gone wrong somewhere.

### The three things that bite

**The far herdr socket is an absolute path on the far machine.** `ssh -L` does
not expand `~` on the remote side, so wsp records the path rather than
discovering it — asking the machine for its `$HOME` would put a blocking round
trip, to a machine that may be down, inside the daemon's tick. `wsp machine add`
writes down this machine's own herdr socket, which is right while the machines
mirror each other and is exactly what a Linux box breaks:

```sh
wsp machine set mb2 backend_at=/home/ed/.config/herdr/herdr.sock
```

The field is `backend_at` and not `herdr_sock` because a machine record says
where its *backend* listens, in whatever words that backend uses — a socket
path for herdr, a host and a port for something reached over TCP. wsp carries
the string and does not read it; the adapter in `src/place_herdr.rs` is the only
thing that knows it names a file, and supplies the mirrored default when the
record leaves it out.

**The shim has to qualify ids.** herdr on the executor numbers its panes from
zero too, so `HERDR_PANE_ID` over there is `w0:p3` and means a different pane on
the seat. `executor/wsp` adds `@$WSP_MACHINE` before handing it across; that is
what `WSP_MACHINE` is for, and it is why the shim is a file in this repository
rather than a one-line alias in the README. It also quotes every argument for
the remote shell — `wsp say "two words"` would otherwise arrive as two — and
passes stdin through, which `wsp edit --overview -` needs.

**The seat's `wsp` must be on a non-interactive PATH.** `ssh host wsp` runs a
non-login shell, and `~/.local/bin` is often not on it. Set `WSP_SEAT_BIN` to
the absolute path if `wsp brief` on the executor comes back "command not found"
about a command that plainly exists.

### What is deliberately not here

- **How code gets to the executor, and how the work comes back.** Undecided.
  Either the agent pushes git→git or the executor works in a worktree, and if
  worktrees win they are built first. Nothing here assumes an answer: the
  executor is handed a cwd and an agent, and how that tree got there is that
  decision's business.
- **Host-qualified roots.** A project root is a path in the store and `~`
  expands on the seat. Correct while paths mirror; the Linux box is what forces
  the change, and the machines file is where it will hang.
- **Affinity.** No `machine` field on a task or project, and no scheduler.
  Placement is asked, never inferred — auto-placement hides the thing you most
  want to see — and because it is explicit, affinity later changes a default
  rather than a design.
- **A wsp-shaped TUI on the executor.** `herdr --remote <target>` already
  attaches a real herdr client to the headless server over ssh, which is the
  "see and manage the agents on that machine" this would be for. A second one
  would be a worse copy.

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
| `~/wsp` (the store) | the same defect as the index, inside the tool: every command committed the whole store, so one agent's `wsp done` carried another's hand-written rule under a message about a task. Fixed in `git_commit` — commands commit the paths they wrote |

### What each check catches, and what it does not

This is the half worth writing down. Every incident happened inside the blind
spot of the check we were relying on at the time.

| Check | Catches | Blind to |
|---|---|---|
| Explicit paths (`git add <file>`) | staging a file you never meant to name | anything already staged in the shared index by someone else |
| Reading `git diff --cached` | a hunk you did not write, if you read the **file list** and not just the hunks | nothing — but only if you actually read it; every swept hunk looked plausible in isolation |
| Isolated build in a worktree | what you wrongly left *out*, and what you took in that only compiles against uncommitted work | whose commit it is; a coherent tree under the wrong author's message compiles perfectly |
| `wsp verify` | the above, without anyone remembering how — and it names what changed that you did *not* put under test | the same blind spot: it proves the patch stands alone, not that the patch is yours |
| A private `GIT_INDEX_FILE` | the other agent's staged work entering your commit | the working tree; two people still editing one file; **the shared index, which nobody is now reading** |
| `wsp doctor` | a shared index holding something older than HEAD, in any declared project root | a shared index holding something *newer* — staged work is indistinguishable from staged work |
| `cmp` build against installed binary | a stale or partial install | nothing, and it is the only reliable one |
| `wsp install` | a second install while one is in flight, and a build older than what is already live | what the live binary *contains*, if somebody installed it by hand — the record beside it describes wsp's own installs and admits when it no longer matches |

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

### `wsp verify`, because step 3 wants to be code

A rule that catches two different agents on two consecutive days is not a rule
anyone is going to start following. `wsp verify [<path>…]` does step 3 —
private index at HEAD, a detached worktree, the patch applied, `cargo test` —
and gets right the parts that are always got wrong. `GIT_INDEX_FILE` is set per
command rather than exported, so `git worktree add` cannot write the new
worktree's index over the private one you just built. The patch comes from `git
add` rather than `git diff HEAD`, so a file git has never seen is still under
test — a new module is the commonest thing an agent adds and the easiest thing
for a diff to miss.

The tree is keyed on the *agent*, not the build, and kept: `CARGO_TARGET_DIR`
sits beside it and persists, so only what you changed rebuilds. Measured on this
repository, 25s cold and 7s warm, of which ~5s is the test run. One tree per
agent to leak rather than one per commit, and `--rm` to drop it.

It also prints what changed that you did **not** name. Naming paths keeps the
other agent's work out of your build, which is the point — and it lets you leave
out something of your own, which fails in the worst direction: a patch holding a
new module that nothing declares compiles perfectly, because nothing compiles
it. That happened while this was being written, a green build in 7s for a change
that did not build. Most of what is listed will be somebody else's and correctly
excluded; the value is seeing at a glance whether one of them is yours.

What it does not do is commit. Steps 1, 2 and 4 are still yours, and the
`read-tree` immediately before the commit is still the one that stops a silent
revert.

### `wsp sandbox`, because a build is not a run

`verify` isolates the build. It does nothing for the verbs that *write* —
`sync`, `reconcile --reap`, `adopt`, `spawn`, the panel's keys — because those
talk to the one herdr everybody is standing in and the one store everybody's
claims are in. Testing them live is how an agent ends another agent's claim
while checking that reaping works.

`wsp sandbox` needs no new mechanism: the three variables wsp has always read,
and a herdr feature we had not noticed.

```sh
herdr --session <name> server
```

brings up a second headless server under `~/.config/herdr/sessions/<name>/`
with its own socket and log, beside the running one and without disturbing it.
With `WSP_HOME` and `WSP_STATE` that is a complete instance: own herdr, own
store, own state. Proved on 2026-08-16 with four agents in this checkout — a
session came up in 0.1s, `wsp reconcile --reap` ran against it, and
`claims.json` and `bindings.json` were byte-identical before and after.

```sh
wsp sandbox                       # up; prints the exports and how to attach
wsp sandbox --seed                # …with the live store's projects and tasks in it
wsp sandbox --run "wsp reconcile --reap && wsp wip"   # one thing, then torn down
wsp sandbox --run "…" --keep      # …and left up to look at
wsp sandbox ls | wsp sandbox rm [--all]
```

Two things make the difference between it working and it quietly not.

**`wsp` inside a sandbox is the binary under test.** The rule is to run the
built binary by path and never `~/.local/bin/wsp` — the install is one file that
every panel, every detail pane and the daemon re-execs into, and making it a
step in testing something puts everyone else's live session downstream of your
experiment. Remembering to type `target/debug/wsp` is exactly the sort of rule
that gets followed for a day, so the sandbox holds a `bin/wsp` symlink at
whichever binary invoked it and puts that directory first on `PATH`. `WSP_BIN`
is exported at the same binary, which is what `herdr-plugin/run.sh` checks
before `~/.local/bin/wsp`.

**It tears the session down.** A cleanup step at the end of a long piece of work
is a step that gets abandoned when something more interesting happens — `git
worktree list` held four stale trees on the day this was written, and an
abandoned herdr session is a worse version of the same thing, because it is a
process. `--run` stops and deletes it whatever the command exited with; `ls`
shows all three states a half-torn-down one can be in — a directory whose
session is gone, a session whose directory is, and a **process** whose session
and directory are both gone; and asking for a sandbox that already exists
replaces it rather than quietly handing back whatever state the last run left.

The herdr it brings up is empty, and manufacturing twenty-two workspaces is not
something this can do. `--seed` copies the store — projects and tasks, so `ls`,
`tree`, `show` and the panel have something to draw — and deliberately not the
machine state, because a claim names a workspace id that exists in nobody's
herdr but the live one.

#### `--fake`, for the states a herdr cannot be put in

That residue is the whole argument for the other kind of sandbox. A herdr that
comes up in 0.1s is not slow; what it cannot do is *be in a state you choose*,
and every expensive bug in this store was a state — an empty pane list reaping
every binding, a `pane.exited` cascade, a machine that stops answering mid-tick,
pane ids reissued across a restart, twenty-two workspaces and four agents.

`wsp sandbox --fake` replaces the one component that has to be real with one
that does not. `src/fake.rs` binds the same socket and answers the same
protocol out of a state written down in a file, so the daemon, the panel, the
storyboard and every command run against it unchanged — nothing downstream can
tell, which is the point.

```sh
wsp sandbox --fake --stage seats.json     # up in 0.0s; prints the same exports
wsp sandbox --fake --run "wsp wip"        # …or one command against that state
```

```json
{ "settle": false,
  "quiet": "no",
  "seats": [
    { "agent": "claude", "name": "t-260816-080", "state": "working", "label": "robustness/080" },
    { "state": "empty", "label": "a shell somebody opened" } ] }
```

The six states are `place.rs`'s — empty, starting, idle, working, gone,
unknown — and the file is live: edit it and the fake diffs the world against
what it was and pushes exactly the events that change would have raised, which
is how the daemon's event path is driven. `quiet` is the state nothing else can
produce: `hangs-up` answers a connection by dropping it, `never` holds it open
and says nothing, and the difference between those two clocks is where
`Err`-is-not-an-empty-list lives.

**The fake is for wsp's reaction to a state; the herdr sandbox stays the
contract check against real behaviour.** A fake that is wrong about herdr makes
tests green on a lie, silently and for ever, so its replies are *generated* from
the state through one mapping function rather than hand-written, and everything
it asserts about herdr was recorded from a live one. Writing it that way caught
two things `place.rs` had wrong on the first run, both since repaired in
`src/place_herdr.rs`: the port decided an agent was starting from
`interactive_ready == Some(false)`, which herdr never sends — for 3.3 measured
seconds it sends the field *absent* with `launch_pending: true` — so the launch
window read as `idle` and a work order would have gone to a pane still drawing
its banner; and the claim that a listing cannot carry `interactive_ready` is
true of `pane.list` and false of `agent.list`, which carries the same record
`agent.get` does.

#### A sandbox's herdr starts the plugins, and they are global

This was got wrong first, and the way it was got wrong is worth more than the
fix. `plugins.json` is one file for the whole machine, not one per session, and
`edjames.wsp` has a `[[startup]]` entry that runs `wsp daemon`. A headless
session server **does** load it. The first live run left a daemon holding that
session's socket and — having been told nothing else — `~/wsp` and
`~/.local/state/wsp`: a process the sandbox created, writing to the live store,
outliving the sandbox, reparented to launchd. Compose that with a `sync` that
reaps every binding when herdr answers with nothing, and running a sandbox ends
every claim on the machine.

Three things follow, and they are not alternatives:

- **The server is started inside the sandbox**, with `WSP_HOME`, `WSP_STATE`,
  `WSP_BIN` and the shim `PATH` already in its environment, because a server
  hands its own environment to every plugin it starts. A process that inherits
  the socket but not the store is the worst of both.
- **Teardown reaps what the session started**, not just the session. `herdr
  session stop` ends the server; its children are reparented and carry on. The
  environment is the only handle — a plugin's daemon has the same command line
  as the real one — so `HERDR_SESSION=<name>` is what is matched, ancestors of
  the process doing the reaping excluded.
- **The daemon refuses a store it was not told about while pointed at a socket
  that is not this machine's**, for the case where the first two are right and
  something starts one anyway. Both halves must be named: `WSP_HOME` alone
  leaves state at `~/.local/state/wsp`, which is where bindings and claims
  actually live. The socket is what decides and `HERDR_SESSION` deliberately
  does not, though it is what the message leads with: the socket path is a fact
  wsp can observe for itself, while the session name is herdr's own vocabulary —
  it calls the main session `default`, and a server that one day exported that
  to its startup plugins would have this refuse the real daemon on every
  restart. Given two signals for one question, prefer the one that survives the
  backend changing.

And the measurement error, because the same reasoning was load-bearing
elsewhere. Two probes had "verified" that a headless session loads no plugins.
Both had pointed `WSP_HOME` at an empty scratch directory to make themselves
safe — so the plugin ran, `wsp daemon` found no store, exited 2, and left
nothing behind. What was then checked was the scratch state directory (empty)
and the server log (no line containing "plugin" — it never prints one either
way). Neither could have shown the process, because `pgrep` was never run. The
redirect that made the probe safe is what suppressed the evidence, and the
absence of an effect was read as the absence of a cause.

### `wsp install`, because it is the one thing nothing isolates

`verify` isolates the build and `sandbox` isolates the run. Neither touches the
install, and no version of either could: `~/.local/bin/wsp` is a single file
that every panel, every detail pane, the daemon and every agent re-execs into
within a tick, and being shared is the entire point of it. It is the one row of
the shared-state table with no isolation story, which leaves only the copy
itself to make safe.

Two agents installing different HEADs a minute apart is a real race, and the
losing side is the quiet one. What ends up live is whichever `install -m 755`
ran second; the agent whose commit it reverted has a green build, a clean `git
log` and no reason to look.

```sh
wsp install                    # what `wsp verify --release` built — HEAD plus your patch
wsp install -n                 # what it would do, and who is holding the lock
wsp install <path> --why "…"   # a binary you name, and a reason worth reading tomorrow
wsp install --to <path>        # somewhere that is not the live one
```

Three things, and it is worth being clear which is the point:

- **The copy is exclusive.** A lock beside the destination —
  `~/.local/bin/.wsp.install.lock`, deliberately not in the state directory,
  because a sandbox replaces the state directory and cannot replace the file —
  held across the copy and nothing else. It waits the two seconds
  `Store::locked` waits and then *refuses* rather than carrying on, which is
  where the two locks part: a lost state update is recoverable, and an install
  that went ahead anyway is the thing this exists to prevent. The refusal names
  the holder, its pid and what it said it was installing, because "held by
  w24:p1 (pid 4123) for 3s — installing fdefcab" is a sentence you can act on
  and `busy` is not.

- **The loser finds out.** Every install is written down beside the binary: who,
  when, at what commit, and the size and mtime of what landed. So the next one
  can say what it is replacing — and refuse, before the copy, when what is live
  was installed *after* the binary in your hand was built. That is the race seen
  from the losing side and the one moment where refusing beats warning.
  `--force` is there for the deliberate rollback, and a newer install of the
  same commit with nothing uncommitted on either side is allowed through: two
  agents building one commit in two trees is not a revert, and a lock people
  work around is a lock they stop using.

- **Nothing execs half a file.** The bytes go to a temporary in the
  destination's own directory and arrive by `rename`, so a running panel holds
  the old inode until it re-execs of its own accord and there is no instant at
  which the path is half a binary. Then the copy is read back and compared,
  which is the `cmp` step 6 of `committing.md` asks for by hand — and a check
  done by hand is a check that gets skipped.

What it does not do is build, and it does not decide *when*. Installing is timed
around the other agents being idle, and that is a judgement no lock replaces;
the lock is for the case where two people made it at once.

What it still cannot answer is what the *live* binary contains. The record
describes what wsp put there and says so plainly when the file no longer matches
it — every install before this command was a hand-run `install -m 755`, which
writes nothing down. The commit stamped into the binary itself is
`t-260816-050`; until that ships, "which of these three commits is live" is
answered by the record beside the file rather than by the file.

### `wsp checkout`, because the tree itself was the shared thing

Every rule above reduces the chance of taking someone's work and none of them
removes it, because they all defend a checkout that belongs to nobody. `wsp
checkout` gives the task in hand a working tree of its own, on a branch of its
own, and `wsp land` puts it back on the trunk. `wsp spawn` does it without being
asked — the lesson this repository keeps re-learning is that a rule an agent has
to remember is a rule it will be halfway past when it matters.

    ~/claude/wsp                       the trunk: what a review reads
    ~/claude/wsp/.worktrees/t-…-022    one agent, one task, one branch

Nested under the root and gitignored, because `project_for_cwd` and `overlap`
place a pane by longest-prefix match against declared project roots: a worktree
outside the root resolves to no project, so `wsp where` goes blank and `overlap`
reads two agents in two worktrees as `Elsewhere` — silencing the warning in
exactly the arrangement this creates. `overlap` learned the matching correction:
a tree under `.worktrees/` is *not* inside the trunk however much the prefix
says so, and it reports `separate tree` rather than a warning it has no business
raising.

Branches are short and land early, because review stays whole-tree (Ed,
2026-08-17). A reviewer reading the trunk against long-lived per-task branches
would be reading a tree none of the work is in. `land` rebases onto the trunk
and fast-forwards the trunk onto that, so the trunk stays linear and never gains
a merge commit from here — and the rebase is where two agents who edited one
file finally meet. That is the arrangement working: the collision that used to
be a silent sweep is now a conflict, in your own tree, with both sides named.

It also prints the diffstat of what actually reached the trunk, which is step 4
of the procedure above and the check that caught a wrong commit on two
consecutive days.

What it costs is a second checkout and a second `target/` per agent, and one
more thing that can be left behind: `wsp checkout --rm` drops a tree, and `land`
drops it for you. The reasoning is in `src/cmd_checkout.rs` and on
`t-260815-022`.

### What none of it fixes

A tree each closes the sweeps and does not close everything. Two agents editing
one file still produce a conflict — a better failure, not no failure. The store
under `~/wsp`, `~/.local/bin/wsp` and the live herdr are shared by design and
have their own answers above. And an agent standing in the trunk, which is where
a person reviewing wants to be, is back in the world every rule above describes.

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
| `src/place.rs` | the place-work port: what wsp asks of whatever runs its agents |
| `src/place_herdr.rs` | that port over herdr: the shell race, the launch window, the retype |
| `src/agent_commands.rs` | the other axis: per-kind flags, the handle wsp mints for an agent, and how it is told |
| `src/arrange.rs` | the arrange-panes port: a desired set of panes, and the reconciler under it |
| `src/draw.rs` | the renderer: one spec and a view, drawn to a terminal or to a block of text |
| `src/fake.rs` | a backend that answers that socket out of a state we choose — `wsp sandbox --fake` |
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
| `src/cmd_checkout.rs` | a working tree per task, and landing it back on the trunk |
| `src/cmd_mandate.rs` | standing direction: what a workspace is for |
| `src/cmd_govern.rs` | the coordinating seat: who answers for a project's raised hands |
| `src/cmd_spawn.rs` | a workspace on a task, an agent started in it, and both ended again |
| `src/cmd_machine.rs` | the machines agents can be run on |
| `src/tunnel.rs` | one ssh per executor, forwarding its herdr socket |
| `executor/wsp` | the shim that stands in for wsp on a machine that has none |
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
  gap between them — process-idle on a `doing` task means a human is the blocker,
  unless the pane holds a seat (`wsp govern`), which is idle between its agents.
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
