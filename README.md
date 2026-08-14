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
| `~/.local/state/wsp/` | bindings, pins, `events.jsonl` — machine-local, not in git |

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
the project tree, tasks hang off projects, agents attach to tasks, and tasks
belonging to no project get an `inbox` heading rather than a footer number.

```sh
wsp panel install            # split it into the workspace you're standing in
wsp panel install --all      # …or every one of them
wsp panel uninstall
```

Install splits with `pane.split` and swaps the new pane into the narrow slot.
It must never go back to `layout.apply`: herdr rebuilds the whole tree from
that call and every pane in it gets a fresh terminal, which takes down any
agent running in the workspace.

Keys: `j`/`k` move, `←`/`→` fold a project, `↵` folds a project or opens a
`⋯ n more` row or jumps to a task's agent, `1`-`9` jump straight to an agent,
`A` shows finished tasks, `r` syncs, `q` quits.

### What the marks mean

| On a task | | On a project | |
|---|---|---|---|
| `●` | agent on it, working | `▾` `▸` | unfolded / folded |
| `○` | agent on it, idle | `7` | open tasks, rolled up |
| `·` | nobody on it | `▸3` | tasks in flight |
| `■` | blocked | `■1` | tasks blocked |
| `◆` | in review | `✓` | all work here is finished |
| `✓` | done — only under `A` | `●2` | agents resolving here |

`←` marks an idle agent on a task that is still `doing` — it has stopped and
you are the blocker; the header carries the count. `⋯ n more` is the tail past
the six-task cap.

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
- **cwd is not identity.** Five workspaces share `~/git/Easter`. Resolution order
  is pin → binding → cwd → workspace label, so `wsp pin <project>` is the
  override when a directory is ambiguous.
- **Tags are inherited.** A task in `trance` also matches `-t juce` and `-t dsp`
  from `vst` and `audio` above it.
