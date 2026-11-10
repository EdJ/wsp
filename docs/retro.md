# The timing retro (wsp-472)

When a worklist clears, wsp opens a **retro seat** on it (a sonnet/high agent, row
"Timing retro of <list>"). It reads the list's effort report (`effort.json`: the wall
in buckets, the biggest stalls, per-row spawn and idle figures) and attributes the
wall to causes within 10 %.

- `wsp retro <list> --packet` prints what the seat reads; `--check` says whether the list
  is eligible. `wsp retro <list>` (or `show`) reads the retro that ran and starts nothing; `wsp retro start <list> [--replaces ROW]` starts one by hand, and is refused over a retro row still open or at review unless named, and from a verifier seat.
- `--no-retro` on a list (or `retro=off`) turns it off. One retro per clear, and only for lists big enough.
- The seat files only through `wsp retro file|note|done`, at most 5 rows a run.
  A **defect** goes on the planned list `retro-N`; a **strategic** question goes on
  `retro-ask-N` with an ask (`--ask FILE`) and waits for a person. A near-duplicate of an open row is
  refused: add the evidence with `wsp retro note` instead.
- Answering the ask moves the approved row onto the list as approved work and plans the list;
  a declined row closes. Once `retro-N` / `retro-ask-N` has started, the next finding opens `-(N+1)`; the started list is not touched.
- Output: `~/.compound/artefacts/<list>/retro.json` and `retro.html`; `wsp yours` carries one line for two days after `done`.

## Standing questions

Questions the retro seat answers on **every** list, whatever the wall looked like. The packet
(`wsp retro <list> --packet`) carries this section verbatim under its own heading. The seat
answers each one for its list in `done` — a sentence or two, with figures from `effort.json` —
and files under `--strategic` only when that list's evidence makes the case.

- **The governor cycle.** How much wall and memory do governor and check seats cost this list while they wait: between groups, while a barrier runs, after a verdict? Seen: an idle Claude Code process holds ~170-230 MB for as long as its seat stays open; governors stay seated between groups; check and retro seats outlived their verdicts (compounding-158, compounding-162; wsp-385 covers part of it); seats waiting on their own shells read as "idle"; lists that read "may start" were not started or seated until a hand ran `go` and `advance` (gate-fix, host-pace). Would a different cycle (seat a governor only when a decision is due, end every seat at its verdict) have cut the wall or the memory, and by how much? (Ed, 2026-11-07)

### Adding or retiring a standing question

Add one as a bullet in this section: a bold name, then the question, what prompted it, and the
date and who asked. Keep it answerable from `effort.json` and the list's logs. It reaches the
packet on the next build, since `src/retro.rs` compiles this file in; the heading must stay
exactly `## Standing questions`, and the section ends at the next `## ` heading (a `###`
subsection stays inside it, so it is carried to the seat too). Retire one by deleting its bullet
once a decision has been made, and say in the commit where the answer went.
