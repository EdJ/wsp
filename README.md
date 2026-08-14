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
| `~/.local/state/wsp/` | claims, bindings, pins, `events.jsonl` — machine-local, not in git |

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

Install splits with `pane.split` and swaps the new pane into the narrow slot.
It must never go back to `layout.apply`: herdr rebuilds the whole tree from
that call and every pane in it gets a fresh terminal, which takes down any
agent running in the workspace.

Keys: `j`/`k` move, `←`/`→` fold, `↵` folds or opens a `⋯ n more` row or jumps
to a task's agent, `1`-`9` jump straight to an agent, `A` shows finished tasks,
`r` syncs, `?` lists the verbs, `q` quits.

### Managing from the panel

| Key | On | Does |
|---|---|---|
| `a` | project, task, inbox | add a task in that scope |
| `P` | anywhere | new project, child of the selected one |
| `s` `v` `d` `o` | task | start, review, done, reopen |
| `b` | task | block, asking why |
| `e` `n` | task | retitle, append a note |
| `m` | task | move — the tree becomes the picker |
| `c` | task or agent | claim, either direction |
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
| `src/panel.rs` | the in-pane sidebar: project tree, tasks, agents |
| `src/cmd_*.rs` | the commands |

Dependencies: `serde_json`. That is the whole list, and it should stay that way —
fast builds are a feature here, because a session-start hook runs this binary.

## Notes

- **Status is work state, not process state.** herdr's `idle`/`working` describes
  the process; `doing`/`blocked`/`review` describes the work. `wsp wip` flags the
  gap between them — process-idle on a `doing` task means a human is the blocker.
- **Claims outlive panes; bindings do not.** A binding is keyed on a pane id,
  the most perishable thing herdr has. A claim names the workspace — id, label
  and cwd, all of which herdr persists — and is cleared only by `release`. A
  pane exiting leaves the claim standing, and `wsp reconcile` rebuilds the
  bindings from claims against whatever is currently open. The daemon does it
  before its first sync, which is when herdr has just restored everything under
  new pane ids.
- **cwd is not identity.** Five workspaces share `~/git/Easter`. Resolution order
  is pin → binding → cwd → workspace label, so `wsp pin <project>` is the
  override when a directory is ambiguous.
- **Tags are inherited.** A task in `trance` also matches `-t juce` and `-t dsp`
  from `vst` and `audio` above it.
