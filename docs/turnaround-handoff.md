# wsp turnaround and phase 2: hand-off

Written 2026-11-20 from Ed's machine, for Jesper's primary to pick up and re-plan on that side. Ed's side has stopped this work. It keeps only the rows that cut the load of web-chrome's poll calls (section 5). Nothing here is filed in your store. Row ids below are **Ed's store's**: his wsp ids and yours share one prefix and number space, and do not refer to the same rows (your wsp-030 and wsp-077 are a README row and a sidebar row in Ed's store). Read every id below as "Ed's store", and quote ids that way in commits that touch this work.

## 1. What is being handed over, and why

Ed: "we got compound to a good rate per ticket, wsp is slow." He then decided to go on **phase 2 of the crate split** (docs/components.md, wsp-378): cut `engine` and `seats` out of the bin crate. His reason: "we found with compound (gpui) that this was a significant time save."

It moves to your side because:
- You are rewriting the code phase 2 moves. sync-2026-11-20 (PR #13) changed `src/repair.rs`, `src/cmd_agent.rs`, `src/ending.rs`, `src/fold.rs`, `src/cmd_spawn.rs`, `src/cycle.rs` and `src/daemon.rs`, and deleted `src/ended_panes.rs`. Cut rows are pure moves that touch every file in flight. With one writer the cuts can be sequenced after the manager work rather than racing it.
- The work is mostly rebuilding a 175k-line crate, and your hardware is faster. On Ed's machine today the CPU was thermally limited to 40 % of its clock for long stretches.

What is handed over: measuring where a wsp row's wall time goes, planning and running phase 2, and ranking the cuts phase 2 does not cover. Re-plan all of it as you see fit. This doc is the evidence and the history, not instructions.

## 2. The baseline so far (rough, from Ed's store and logs)

These figures cover rows landed from 2026-11-06 to 2026-11-20. They were read off task logs and the state logs and are meant to be redone properly.

| | wsp | compounding | compound |
|---|---|---|---|
| start to landed, median / p75 | 24 / 43 min (229 rows) | 22 / 58 min (582) | 12 / 26 min (22) |
| share of rows sent back at least once | 42 % | 49 % | 23 % |
| a row's own `wsp check`, median / p75 (steps that ran) | 96 / 227 s (209) | 43 / 116 s (1,159) | |
| barrier run, median / p75 | 378 / 574 s (59, wsp's lists) | 185 / 465 s (229, every other list) | |

What this suggests is that an agent's own work takes about the same time in every project. wsp is slow where it builds and tests: its checks and barriers take about twice as long. The likely root:
- `src/` is still one crate of 175k lines in 107 files. The largest are cmd_worklist.rs (12.0k), cycle.rs (11.5k), cmd_spawn.rs (11.3k) and cmd_agent.rs (11.0k).
- It has 2,531 `#[test]`s in one test binary, so any edit recompiles that crate and relinks that binary.
- A release build of wsp alone took 2 min 29 s.
- Load makes it worse. check-set's barrier 1 took 1,424 s at load 95 against 273 s at load 4.

Not yet measured, and planned on Ed's side as wsp-699's two measuring legs:
- **Where the wall goes.** Split at least 20 wsp rows and 10 compound or compounding rows into stages: seat start, the agent's own work, `wsp check`, the verifier, rebase and land, and the barrier. Use `wsp timeline`, `wsp attempts --cost` and the effort reports, with the load each ran at.
- **The inner loop.** At load below 4, three runs each:
  - a warm one-line edit in `src/cycle.rs` to `cargo test --bin wsp cycle::`;
  - the same in one crate under `crates/`;
  - the bin crate's test-binary build, front end against codegen and link;
  - `wsp check --affected --run`, and the full suite as the barrier runs it.

For the before and after, measure both on the machine that does the cuts.

## 3. Phase 2: the existing plan and its gates

The plan is docs/components.md (wsp-378, the components list, closed 2026-11-07).
- **Phases 0 and 1 are done:** the seams p0 to p6, then the cuts that made base, testkit, store, store-contract, wire and trees.
- **Phase 2 is `engine` and `seats`,** 63k lines and half the code at the time. Section 5 gated it on two things:
  - p5 (wsp-406, landed 2026-11-07) holding for a fortnight;
  - the contract-change share over the 20 `crates/` commits after wsp-412 being under about 35 %. Above that, the gain is under 40 % and the plan stops at six crates.
- **The share was never properly measured.** wsp-474 counted 5 of 20 commits on 2026-11-07 (0 of 5) and was owed a re-run. A rough count on 2026-11-20: 240 `crates/` commits since 2e96914 (wsp-412), of which 6 touch `crates/store-contract` or `crates/api`. That is about 2.5 %. The api crate and the new crates (server, client, manager, view) muddy it. Ed treats the gates as met.
- **The module sets in §5 date from 2026-11-07.** Your manager work and wsp-view (wsp-305 on your side) have moved a lot since. Re-derive the `engine` and `seats` sets from today's `src/`.
- **The method that worked for phases 0 and 1:**
  - seams first, until the upward edges are zero (`tests/layering.rs` is the ratchet);
  - then one crate per row, as pure moves with `git mv`;
  - a `crates/<name>/MAP.md` per crate, under 60 lines;
  - check scripts and `affected` keys renamed in the same row;
  - `serde_json` the only external dependency, with `rusqlite` allowed in `wsp-store-sqlite` alone.
- **Rows in Ed's store that wait on phase 2.** Re-file or re-shape them on your side:
  - **wsp-460** (todo), "Seams: store's upward edges to zero, before its cut". It is the parent of the store seam rows, from Ed's 2026-11-07 re-plan bottom-up: a module becomes a crate only once everything it uses is a crate.
  - **wsp-409** (parked), "Cut 2: wsp-surface (panel, detail, draw, kanban, live, story, arrange)". About 21k lines and 3 edges at the time. Its gate: edit wsp-surface once and time `cargo test --no-run`; it must come in under half the 21 to 32 s measured on 2026-11-05 for one edit in the single crate. It pairs with robustness-082 (the surface as its own `[[bin]]`). It can only be measured once engine and seats are crates.
  - **wsp-410** (parked), "Cut 3: wsp-timeline (timeline, cost, burn, report, attempts, verifications)". About 3.8k lines and 7 edges, reads only, with the same gate.
- **Compound's lessons** are in Ed's compounding repo: `docs/minimum-loop.md`, `docs/fast-loop/compound-971.md`, `docs/fast-loop/compound-972.md` and `docs/fast-loop/thermal.md`.
  - The minimum loop: 0.70 s from edit to verified for a widget built as its own small crate and checked by text snapshots, against 3.46 s for an edit in a large crate. About two thirds of the small-crate loop is fixed cost (first-exec checks, rustc start-up).
  - The fast-loop notes cover a gpui dylib for test builds, first-exec cost, and narrow `cargo test -p <crate> --lib <module>` loops.

## 4. Cuts phase 2 does not cover

These were to be ranked on Ed's side, each with its expected minutes per row:
- **Narrower check steps** (`wsp check --affected`). Every row's own checks and the barrier currently tend to the full suite.
- **Sharding or splitting the test binary,** independent of the crate split.
- **The verifier's and the barrier's share of the wall.**
- **wsp-414** (todo), barrier latency: run independent groups beside a group waiting on its barrier. It builds on worklist lanes (wsp-363) and integration branches (wsp-362), both parked. It is a design for filing, to settle with Ed.
- **Thermal and load.** The throttle holds checks at 4 machine-wide when the clock is limited. On 2026-11-19, load of about 800 on 8 cores slowed everything and preceded the incident in section 6.

## 5. What Ed's side keeps running

These are the poll calls web-chrome's census makes every few seconds: `wip --json`, `ls --json --all` and `worklist show --all --json`. Together they were about a core of steady load. They run as list wsp-read-api in Ed's store. Coordinate with it so the two sides do not do the same work twice.
- **Done:**
  - wsp-692: the client finds a frame's end in linear time (00a8cc2).
  - wsp-693: two tests that failed only under load (6b26cb5).
  - wsp-698: wip_api's load check (9bfa6ec).

  These are on Ed's master and not yet on origin. Ed's master and origin/master need merging; that is owed on Ed's side.
- **Next:**
  - wsp-695: a wip-shaped read under MAX_LINE. On Ed's store `state` is 51.4 MB, past the 16 MiB MAX_LINE.
  - wsp-697: a survey of every whole-state reply over MAX_LINE.
  - then wsp-688 (`ls --all` from the API), wsp-689 (`worklist show --all` from the API) and wsp-696 (`wip` over the API meets state-core step 1's bar, and becomes the default again).
- **Overlap with your side:**
  - Your state-core.md step 1 result found "the API wip is 11x slower than the files path". That is the same problem wsp-696 is working.
  - wsp-688 and wsp-689 must build on wsp-view, which now shapes `ls` and `show`.
  - Measured on 2026-11-20: `ls --json --all` takes 1.59 s and gives 3.2 MB of head fields only. `worklist show --all --json` takes 2.04 s and gives 2.9 MB, overviews included.

Also queued on Ed's side, as incident fixes rather than optimization (list incident-11-19):
- wsp-690: a send-back refused at the leg cap tells the list's governor.
- wsp-691: seat clean-up is automatic. A broad `pkill`/`killall` is refused (on macOS, options after the pattern become patterns), and what a finished member leaves running in its tree is stopped.
- wsp-694: a held spool is capped and summarised.

wsp-691 and wsp-694 touch `src/ending.rs`, `src/cmd_watch.rs` and `src/wake.rs`, which your manager work rewrote. Ed's side rebases them onto origin/master first. Say if any of this already moved to the manager, so they are written against the right place.

## 6. Context: the 2026-11-19 incident

At 12:32:32Z a verifier ran `pkill -f "scripts/check.mjs" -P 0` at load of about 800. On macOS this matches every process with a `0` in its command line, so it ended every Claude session on the machine:
- the primary and all governors;
- members and verifiers;
- sessions that had been idle for days.

The panes stayed open and empty. The run then stalled for about 14 hours for these reasons:
- **A row hit the leg cap.** compounding-760 used its 8th leg, and the governor was never told.
- **The custodian was wedged.** It had a 515-item, 81 KB held backlog typed into its input box as one message.
- **The manager crash-looped.** After the 2026-11-20 merge, wsp-manager kept failing on a tailnet address the Mac does not hold. Fixed in 3488325, as 9957f8e had fixed the server.

Ed's side has fixed or filed all of it (section 5). It explains the noisy load figures from 2026-11-19 in any effort report you read.
