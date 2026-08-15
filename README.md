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
| `~/.local/state/wsp/` | claims, bindings, pins, `worked.json`, `events.jsonl` — machine-local, not in git |

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
tab, `1`-`9` jump straight to an agent, `A` shows finished tasks, `r` syncs,
`?` opens the key map.

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
| `a` | project, task, inbox | add a task in that scope |
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
the six-task cap.

The inbox sits at the top: unfiled work is what you triage before reading
anything that already has a home. Loose agents sit at the foot, after the work.
Both fold like a project and take the cursor like one, so a command can be
aimed at them.

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
a command that sets it correctly. The body is yours, and carries three sections:

| Section | For |
|---|---|
| `## Overview` | what the task is, written once, read to re-enter it |
| `## Details` | working material — criteria, links, whatever the work needs |
| `## Log` | dated, append-only; `wsp note` writes it, nothing edits it |

```sh
wsp edit <id>                    # both sections, headings included
wsp edit <id> --overview         # just that prose
wsp edit <id> --details
wsp edit <id> --raw              # the whole file, when the frontmatter is wrong
wsp project edit <id> [--overview|--details|--raw]
```

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
| `src/panel.rs` | the in-pane sidebar: project tree, tasks, panes |
| `src/detail.rs` | the detail pane: one task or project, in full |
| `src/cmd_*.rs` | the commands |

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
- **Tags are inherited.** A task in `trance` also matches `-t juce` and `-t dsp`
  from `vst` and `audio` above it.
