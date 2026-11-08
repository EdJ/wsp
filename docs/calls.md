# What a wsp call costs (`wsp-481`)

Ed, 2026-11-07, after `wip --json` measured 12.6 s and 9.4 CPU-s per call: "add a
feature to wsp to record the times of its calls, so we can start to optimize
those". Before this, nothing recorded it; wsp-480 was found by hand with
`/usr/bin/time` and `sample`.

## Two files: actions and polls (`wsp-488`)

Ed, 2026-11-07, on the one file: "maybe we need separate poll and non-poll logs,
actually". Polls and actions are different measurements. A poll (compound's host
reading `wip --json`, `worklist ls --json`, `ls --json --all`, `stamp --json`
every half minute) matters as a rate and a total CPU per caller; an act (`tell`,
`spawn`, `review`, `verify`, `worklist go`, the daemon's ticks) matters one by
one. In one file a poll storm pushed the action history out of the window.

| file | holds | cap |
|---|---|---|
| `calls.jsonl` (+ `calls.1.jsonl`) | actions, and the daemon's ticks | 20 MB, one rotation |
| `polls.jsonl` (+ `polls.1.jsonl`) | polls | 20 MB, one rotation |

**A call is a poll when**

1. its caller says so: `WSP_POLL=1` in its environment, whatever the verb and
   whoever runs it (the way for compound's host, or any client that polls, to
   be sure of it); or
2. its verb is **read-only** and it is **run by no seat** (`role` is `none`: no
   `WSP_SEAT_ID`, `HERDR_PANE_ID`, task or verdict in its environment).

A seat running a read-only verb is an agent working, so its `wsp show` stays in
`calls.jsonl`. A verb that writes, even for some forms, is an act wherever it
runs. Daemon ticks are never polls.

**Read-only verbs** (`src/readonly.rs`, the one place; `every_verb_is_classified`
fails on a verb in neither list, so a new verb must be decided): `wip`/`status`,
`ls`/`list`, `find`/`search`, `show`/`cat`, `inbox`, `stamp`, `where`, `peek`,
`overlap`, `timeline`, `tree`, `projects`, `burn`, `calls`, and
`worklist ls` / `worklist show` (`worklist` otherwise writes). They write nothing
to the store or the state directory beyond this log. The table errs toward
actions: a verb not sure to be read-only is listed with the acts.

**One line per poll, not folded.** Folding to a line per minute per (caller,
verb) was the option if a line per poll would turn `polls.jsonl` over in under
a day. It would not: a call line is about 300 B (the real `calls.jsonl` holds
907 kB in 3,108 lines, 292 B each), and compound's host at the measured rate is
about 4 calls per 30 s, 11,500 a day, about 3.5 MB a day, so the 20 MB file
holds about 5.8 days and with its rotation about 11. Before compound-996 the
rate was "far more"; at ten times it would be 35 MB a day, still about 1.2 days
for the two generations. A fold would need a read-modify-write of the last
line under a lock, which the `O_APPEND` write avoids, and would lose the
per-call p50/p95 that `wsp calls --polls` reports, so it is not done. If the
rate climbs past that, fold then.

## What is recorded

Both files hold the same two kinds of line (`call` lines go to either; `tick`
lines only to `calls.jsonl`). Both are one `write` on an `O_APPEND` descriptor, so there is no lock and
no interleaving inside a line; a torn line is skipped by the reader.

**`"k":"call"`** — one per `wsp` process, written as it exits (an `atexit` hook,
so `process::exit` anywhere is covered; the exit code is remembered by
`wsp_base::calls::exit`, which replaces `std::process::exit` in `main`):

| field | |
|---|---|
| `t` | start, UTC |
| `verb`, `sub` | the verb, and a sub-verb only when the help documents `wsp <verb> <word>` — `worklist next`, never `tell cpd-3` |
| `flags` | flag **names** only; never values, prose, ids or file contents |
| `seat`, `role` | the seat id, and primary / governor / member / verifier / check / daemon / seat / none — from the governors record and the `WSP_VERDICT` kind wsp set, never from anything the caller says |
| `parent` | the parent's command name (`claude`, `zsh`, a compound host), read from the kernel, not from `ps` |
| `wall_ms` | from the start of `main` |
| `user_ms`, `sys_ms` | `getrusage(RUSAGE_SELF)` |
| `kids_user_ms`, `kids_sys_ms` | `getrusage(RUSAGE_CHILDREN)` |
| `children` | per program: `n` runs, `ms` total wall |
| `exit` | the exit code |

**`"k":"tick"`** — one per pass of the daemon's loop: `wall_ms`, CPU, `children`
for that tick, and `phases`, ms per named phase (`tunnels`, `attention`, `cycle`,
`closing`, `barrier`, `ending`, `ending:verification`, `sweep:*`, `seats`,
`pickup`, `retro`, `fingerprint`, `sync`, …). A phase runs until the next
`wsp_base::calls::phase("…")`, so adding a mark in a new place splits a phase by
name; the daemon process itself writes no `call` line.

## The one place a child is counted

`wsp_base::calls::Command` is a drop-in for `std::process::Command` (same builder
methods, plus `process_group`, `pre_exec`, `exec`). `output` and `status` time the
run; `spawn` counts the run and adds its wall when it is waited on. Every
`Command::new` in `src/` and `crates/` goes through it. **Use it, not
`std::process::Command`, for anything new** — a straggler still shows in the CPU
totals (they come from `getrusage`) but not in the split by program.

## Reading it

```
wsp calls [--since 1h] [--by verb|caller|child|phase] [--polls|--all] [--json]
```

By default `wsp calls` reads the actions (`calls.jsonl`), `--polls` the polls
(`polls.jsonl`), `--all` both together. The heading names which. `--by phase`
is the daemon's ticks, which are only in `calls.jsonl`.

One line per verb (`wip --json` is its own line), caller (`member via claude`) or
child program: count, p50 and p95 wall, CPU-s and share of all wsp CPU in the
window, dearest first. `--by verb` and `--by caller` print each of the top rows'
children under it. `--by child` ranks by wall (CPU is not split by program).
`--by phase` and the end of the default report show the daemon's ticks.

Every report, text and `--json` (`covers`, an object, or an array of two under
`--all`), says what each file it read really covers: the file's name, the
oldest and newest record read, the sizes of its two generations, and — when the oldest
record is newer than the start of the window asked for — `SHORT: asked for 2d …`,
so a rotated or new file cannot pass for the whole window.

## Keeping it cheap

`WSP_NO_CALLS=1` turns it off. Each file rotates to its `.1.jsonl` at 20 MB, so
each holds at most about 40 MB (a test drives it past several caps and checks the older generation is dropped and live + old stay under 2 × cap). A record costs about 0.1 ms
(`the_record_an_exit_writes_costs_under_a_millisecond`; `--release --nocapture`
prints it). Under test builds, nothing is written unless `WSP_STATE` is set, so
a test that was not isolated cannot reach the real file.
