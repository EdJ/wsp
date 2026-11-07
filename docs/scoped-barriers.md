# Barriers scoped to what the group touched

`wsp-431`, 2026-11-06. Builds on the barrier run (`wsp-326`, `docs/barrier.md`), the greens ledger and
input-hash reuse (`wsp-342`, `wsp-361`, `src/affected.rs`).

Ed, 2026-11-06: *"if we're doing components, we likely don't need to run full-suite tests so often, even at barriers
(assuming no files are touched that alter multiple components)."* Pass 1's barrier runs were 6.4 minutes a group, and
81 % of four runs was two `cargo test` steps for code the rows had already tested (compounding's
`docs/row-cost.md` §2).

## The rule

A barrier's **full-suite steps** are narrowed to the targets the group's changed files reach, when the group's kind
says its work is local and nothing it touched fans out. Everything else runs as it did. A doubt is a full run.

| the group | the barrier runs |
|---|---|
| the list's **last** group | the full suite, always |
| kind `insitu` | the full suite, then the drive |
| kind `contract` | the full suite: the contract and types fan out to everything |
| **no kind** | the full suite, as before: nothing says it may be narrower |
| kind `components` or `wiring`, a **global** path changed | the full suite |
| kind `components` or `wiring`, a **safety-net** barrier is due | the full suite |
| kind `components` or `wiring`, a **shared** path changed | the targets of that path's **dependents** (widened) |
| kind `components` or `wiring`, unit files only | the targets of the units touched |
| any narrow group whose files the graph cannot place | the full suite (the graph reaches nothing for them) |

`wsp worklist group <list> N --kind contract|components|wiring|insitu|none` sets the kind. It is read at the barrier and
by nothing in flight, so it is a free edit like the caption. The line is `kind:` in the group, and **a wsp from before
this reads it as more members** (`wsp install` before a list carrying one is run by an older binary). The kind feeds
auto-pass (`wsp-396`) only in that a last group and an in-situ group are never narrowed; a narrowed barrier passes by
the rule it passed by before.

## Units, shared paths, and what is derived

The *changed files* are observed from the landed commits (`worklist::touched`), never declared. Units come from the
graphs, not a list:

- **Rust:** `cargo metadata --no-deps`; a crate a file changed in runs `cargo test -p <crate>`, then its reverse
  dependents (`affected::reached`).
- **React / TypeScript:** `npx vitest related --run <files>`, Vitest's own module graph, which already runs the
  dependents of a shared module.
- **Prose** (`*.md`, `docs/`) reaches no test.

A project names what is **not** derived, under `## Barrier` in its body:

```text
shared: src/types/**, src/services/**
global: scripts/ci.sh
full-every: 5
```

- `shared:` globs fan out. Nothing is added to the run beyond the graph's own dependents; the paths are named in the
  report as what widened it. (`wsp-414`'s lanes own paths, and a lane is a unit in this sense: a directory with its
  tests that the graph can name; one notion of a unit, so the lane's path set and this scope's units are read from the
  same graphs and the same `shared:` list.)
- `global:` globs are full-suite paths beside the built-ins: `Cargo.lock`, `Cargo.toml` and `package.json` at the
  root, `package-lock.json`, `pnpm-lock.yaml`, `yarn.lock`, `build.rs`, `rust-toolchain*`, `.cargo/`, `tsconfig*`,
  `vite.config.*`, `vitest.config.*`, `webpack.config.*`.
- `full-every: N` is the trunk's safety net (below). Five by default; `0` turns it off.

## What changes in the run (`barrier_run.rs`, `scope.rs`)

1. `run` reads the landed files and the project's `## Barrier` lines, and [`scope::decide`] gives the **scope**: full
   with a reason, or narrow.
2. Each unit's steps are narrowed *in the barrier tree at the trunk's tip* (`scope::narrow`), because the targets come
   from that tree's graphs. A step is replaced only if it is a whole suite (`cargo test` with no filter, `npm test`,
   `npx vitest run`); a stop check, a filtered command, a member's own script stay as written. The one exception is the
   React whole suite (`wsp-468`): `npm run check`, or `npm run check:box` with no box, optionally after `cd <dir> &&`,
   even as a stop check, is replaced in a narrow scope by `cd <dir> && npm run check:box -- <box>` per box touched
   (`<dir>/src/<box>/…` in the landed files). It stays whole when a `shared:` path, `src/contract|services|shell/`,
   the package manifest or any file in the package outside a box changed, or nothing in the package can be placed;
   and on the last group and the in-situ group, as every full scope. A member's recorded check
   that *is* the suite is narrowed with it and keeps its owners, so blame is unchanged. A target two steps name runs
   once.
3. The results are read against the steps that ran (`effective` items), so blame, the failed list and the packet agree
   with what ran. The packet's first line is `Scope: …`.

## The greens ledger

Narrowed targets are plain `cargo test -p <crate>` commands, so they take the ledger's input-hash key
(`affected::key`): a unit green at the same inputs is **reused** across commits, and a unit the rows already proved is
not run again. Scoping and reuse multiply. A step that stays whole (a stop check, a full suite on a last group) is keyed
as before.

## The trunk safety net

The graph can miss (a runtime path, a generated file, a test that reads another crate's output). So:

- the full suite runs at the batch's in-situ step and a list's last group (always), whenever a global path changed, and
  **every `full-every` scoped barriers** on the project (`barrier/<project>-scope.json` holds the count and the
  seconds the last full run took);
- a **full run that is red with scoped barriers behind it** is a *safety-net red*: the log line says
  `SAFETY-NET RED behind scoped <list>/<n> …` and the group's scope note keeps the list. The suite's own blame (by the
  files the output names) files it against a landing; the scoped barriers it stood behind are what scoping let through.

## Effort report

`wsp report` per group, `barrier_scope`: `scoped`, `why` (a full run's reason), `targets`, `widened`, `seconds`,
`full_seconds` (the last full run's, when one has run) and `seconds_saved` (the full run's seconds less this one's, for
a scoped run). Totals: `scoped_barriers`, `full_barriers`, `scoped_seconds_saved`, `safety_net_reds`,
`safety_net_misses` (group and the scoped barriers behind it). The cycle log's `barrier run —` line ends
`· scope: …` after the per-step figures.

## Not in this row

Rust components beyond crates (the components list), a `wiring` group's screens named by anything beyond the module
graph, a nightly full run on the trunk (the count of scoped barriers is the net for now) and filing a red against the
landing automatically (the red names the landings, in the log and the report).
