# The barrier, run by wsp where nothing needs judgment

`wsp-326`, 2026-11-01. Builds on `wsp check` (`wsp-324`) and the verdict records
(`wsp-325`).

Ed, 2026-11-01. Of 186 barrier-check transcripts the median is 9 turns and 0.54M
tokens; 24% of the calls build and test, 20% inspect git, 19% read wsp state to
gather members and verdicts, and 0.2% drive the app. A barrier does three jobs:
**integration** (do the members hold together on the trunk), the **stop
condition** (partly mechanical, partly judgment), and **steering the run**
(follow-ups, rewriting later groups). Only the third needs an agent every time.

## What wsp runs

When a group's last member has landed and every verdict is in, the step that used
to start the barrier's agent first starts a process of its own
(`wsp worklist advance <list> --barrier-run <n>`), because the suite takes
minutes and the tick does not wait. For each repository the group's members live
in, at the trunk's tip, in the warm tree `wsp check` keeps:

1. the project's **`at barrier` steps**: the full suite, once for the group;
2. every member's **recorded `## Checks`**, replayed on the integrated trunk;
3. the group's **`stop-checks`**: the mechanical half of the stop condition.

A command two of these name runs once. **Every step runs** whatever the one
before did, because a red suite is no reason not to read a member's own check.
A step that fails once is run once more, as every `wsp check` step is, and a
flake stays on the entry.

The entry on the group says where it is: `suite:running` from the moment it is
opened (so the retake rule waits hours, not the ten minutes a bare start does),
then `suite:green` or `suite:red`. A start that failed after the suite finished
retries the start and does not run the suite again.

**The run is not a try.** `wsp-328`'s backoff (three starts of the agent, three
then five minutes apart) counts the agent's starts only. An entry with
`suite:running` is believed for three hours before it is taken again, and taking
it again runs the suite again without spending a try; an entry whose suite
finished and whose agent never came up is retried on the backoff, and does not run
the suite twice.

### The stop condition's `checks:` part

```text
- 1  compound-770  compound-771
  stop: flag Ed if the cost doubles.
  stop-checks:
      - gold (20m): cargo test -p host --test golden
      - bound: cargo test -p host-surface source_bound
```

`wsp worklist stop-checks <list> <n> --from FILE` sets them (one `- name
(limit): command` per line; `--drop` clears). The word on the group is
`stop-checks:` and not `checks:`, which is already the entries' block. Until the
barrier's check is open they may be changed, as the stop may. **A wsp from before
this reads these lines as more members**: `wsp install` before a list carrying
one is run by an older binary.

### Blame

A failed step is attributed to a member:

- a member's own recorded check that fails is that member's;
- a suite or stop step that fails is named for the members who **landed a file the
  output mentions**, by its path, its file name, or for a Rust file its module
  (`store::`, the form a test failure is written in);
- one that names no file a member landed is said to be **unattributed**.

## When no agent is asked

wsp passes the barrier, through `wsp worklist go`, when **nothing needs judgment**:

- every member's newest verdict holds, and every item it `checked:` was held;
- at least one check was declared, every one ran, and every one passed;
- the group's stop has **no prose**.

The verdict names what ran and where the results are; the group's entry reads
`passes … suite:green by:wsp`, and `cycle.log` has a line saying why. Anything
else starts the agent as before, with the **packet** in its work order: each
member and its verdict, the stop's mechanical items, and what wsp ran with whose
fault each failure reads as. The order tells it not to build or run what wsp
already ran. A barrier with nothing declared at all is never passed by wsp: with
nothing run, nobody has looked.

**It is off until a project turns it on**, with `auto-pass: on` under a
`## Barrier` heading in the project's body, and every project a group ran in must
have. Off, wsp still runs everything, still hands the agent the packet, and logs
`would pass without an agent (auto-pass is off)` for each barrier it would have
taken — the measure of what turning it on would do.

## The replay

`wsp worklist replay [<list>…]` reads the recorded barriers and never writes.
Run on Ed's store on 2026-11-01: **44 lists, 239 barriers with a check recorded,
198 that passed, 29 that held** (a hold, or a hold pending follow-ups) and **32
that passed with follow-ups**.

A barrier already on record cannot be re-run, since only `wsp` declares checks at
all (`build`, `module`, and `full at barrier`; compound declares none), so the
replay asks what the record does hold: the stop's prose, and the reason each hold
gave.

| the 29 holds | |
|---|---|
| the stop had prose; an agent reads it whatever else is true | 26 |
| no prose, and the reason is a red build or test | 3 |
| no prose, and the reason is something else | **0** |

- **No hold would have slipped past the rule.** Twenty-six could not have passed
  without an agent. The other three are `issue-popout` 4 (7 tidy tests and
  `source_bound` red on the trunk, which no member's own run showed), `sidebar-2`
  8 (two golden tests red; "the PNG golden suite only runs at the gate lane") and
  `ui-to-components` 8 (`source_bound` red). Each is a red suite on the integrated
  trunk, so a project that declares those lanes at its barrier would have been held
  by the check and not by an agent. **That is a condition**: in each the members'
  own narrower runs were green, and the reason was the lane nobody ran. Compound
  declares nothing today, so it would have been held by the rule's other half
  (nothing declared, an agent) until it does.
- **Most barriers have prose, and are not the automatic pass's.** Of the 198 that
  passed, 25 had no stop prose. Under today's habit of writing the stop as a
  sentence, the rule takes about one barrier in eight off an agent; the rest only
  gain the packet. Moving a stop's mechanical items into `stop-checks` is what
  widens it.
- **What it would not have found is steering.** Of the 32 that passed with
  follow-ups, 7 had no stop prose, so an automatic pass would have taken them. Read
  by their follow-ups' titles: three were red tests the rule would have caught
  (`tweaks-next` 1, `gpui-migration` 14 and `issue-popout` 5, each with a failing
  window or golden test); four carried only what nothing mechanical sees — a stale
  hold in a comment (`compound-known-issues` 4), a doc naming a deleted type
  (`ui-to-components` 9), tests that set a process-wide variable and never restore
  it (`ui-to-components` 11), and a stale gallery plus a hung window binary
  (`tweaks-next` 3). Those four passed with small follow-ups and would have passed
  with none. That is a cost of the automatic pass, not a hold slipping, and it is
  the case for leaving the project's switch to the person.

**Decision.** The replay found no hold the rule would have let through, so the
automatic pass is allowed. It ships **off**: turning it on is a line in a
project's body, the only project declaring a barrier suite is `wsp`, and the
follow-ups it would not have found are a cost Ed should choose. For `wsp`:

```text
## Barrier
auto-pass: on
```

The first real signal is the log: with it off, each barrier `wsp` would have
passed says so in `cycle.log`, and a week of those against what the agents found
at the same barriers is the second replay.
