# What a wsp call costs (`wsp-481`)

Ed, 2026-11-07, after `wip --json` measured 12.6 s and 9.4 CPU-s per call: "add a
feature to wsp to record the times of its calls, so we can start to optimize
those". Before this, nothing recorded it; wsp-480 was found by hand with
`/usr/bin/time` and `sample`.

## What is recorded

One file, `$WSP_STATE/calls.jsonl` (`~/.local/state/wsp/calls.jsonl`), two kinds
of line. Both are one `write` on an `O_APPEND` descriptor, so there is no lock and
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
wsp calls [--since 1h] [--by verb|caller|child|phase] [--json]
```

One line per verb (`wip --json` is its own line), caller (`member via claude`) or
child program: count, p50 and p95 wall, CPU-s and share of all wsp CPU in the
window, dearest first. `--by verb` and `--by caller` print each of the top rows'
children under it. `--by child` ranks by wall (CPU is not split by program).
`--by phase` and the end of the default report show the daemon's ticks.

Every report, text and `--json` (`covers`), says what the file really covers: the
oldest and newest record read, the sizes of both files, and — when the oldest
record is newer than the start of the window asked for — `SHORT: asked for 2d …`,
so a rotated or new file cannot pass for the whole window.

## Keeping it cheap

`WSP_NO_CALLS=1` turns it off. The file rotates to `calls.1.jsonl` at 20 MB, so
it holds at most about 40 MB (a test drives it past several caps and checks the older generation is dropped and live + old stay under 2 × cap). A record costs about 0.1 ms
(`the_record_an_exit_writes_costs_under_a_millisecond`; `--release --nocapture`
prints it). Under test builds, nothing is written unless `WSP_STATE` is set, so
a test that was not isolated cannot reach the real file.
