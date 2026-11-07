# The timing retro (wsp-472)

When a worklist clears, wsp opens a **retro seat** on it (a sonnet/high agent, row
"Timing retro of <list>"). It reads the list's effort report (`effort.json`: the wall
in buckets, the biggest stalls, per-row spawn and idle figures) and attributes the
wall to causes within 10 %.

- `wsp retro <list> --packet` prints what the seat reads; `--check` says whether the list
  is eligible. `wsp retro <list>` by hand starts one.
- `--no-retro` on a list (or `retro=off`) turns it off. One retro per clear, and only for lists big enough.
- The seat files only through `wsp retro file|note|done`, at most 5 rows a run.
  A **defect** goes on the planned list `retro-N`; a **strategic** question goes on
  `retro-ask-N` with an ask (`--ask FILE`) and waits for a person. A near-duplicate of an open row is
  refused: add the evidence with `wsp retro note` instead.
- Answering the ask moves the approved row onto the list as approved work and plans the list;
  a declined row closes. Once `retro-N` / `retro-ask-N` has started, the next finding opens `-(N+1)`; the started list is not touched.
- Output: `~/.compound/artefacts/<list>/retro.json` and `retro.html`; `wsp yours` carries one line for two days after `done`.
