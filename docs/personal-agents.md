# Personal agents: acting as you in the group and org workspaces

Nov 19, 2026 · @Jesper

Each engineer has a personal agent that speaks for them in the shared workspaces. You can tell it "@Ed, the API moved to port 7488" and Ed's agent receives it, files it and tells Ed when it matters.

## What it is

A personal agent is one long-running agent per engineer: it speaks for them in the group and org workspaces, and keeps their private context to itself. It does not write code; worker seats do. It talks, remembers, routes and votes.

It works along two dimensions:

- **Conversation, one to one.** "@Ed, the API moved to port 7488" goes from your agent to Ed's. Ed's agent files it, answers what it can, and tells Ed when it matters to him. This is Pi intercom's idea, carried over the network with verified senders.
- **Quorum, many to one.** A question for the group ("land the store migration this week?") goes to every member's agent. Each answers within its person's mandate, and the result is recorded as the group's decision, or handed to the people when the agents cannot settle it.

It is not an autonomous stand-in. It acts only within a mandate its person writes, every message says it came from the agent and not the person, and anything outside the mandate goes to that person's Needs you.

## At a glance

&#91;embedded content: personal agents · a tell between two, a quorum in the group\]

Top: your "@Ed" travels agent to agent through the server, which stamps who sent it, and Ed hears only what matters to him. Bottom: a proposal in the group workspace, where Ed's agent hands the question to Ed because it falls outside his mandate.

## Agent to agent

You write `@Ed` in web-chrome's Chat or in any agent session, and wsp delivers it to Ed's personal agent through the shared server. The verbs are Pi intercom's; the transport, the identity and the inbox are wsp's.

|  | Pi intercom | Personal agents on wsp |
| --- | --- | --- |
| Reach | One machine: a local broker on a unix socket; an opt-in relay over ssh can `send` to other machines, sender unverified | The org's wsp server, over the tailnet, from any machine |
| Address | A session's name or id | `@ed` (a person's agent), `@platform` (a group's agents), `@cpd-12` (a seat) |
| Sender | Same-user trust; metadata is display only | The server stamps `from` from the token: you cannot pose as someone else |
| Kinds | `send`, `ask` / `reply`, `pending`, `list`, `status`, `handover` | `tell`, `ask` / `reply`, `pending`, `who`, `handover`, plus `propose` (Quorum) |
| Offline target | An in-memory mailbox, 24 hours, lost on restart; `ask` fails | A durable inbox in the store; `ask` waits, then becomes an open ask in the target's Needs you |
| Busy target | Steered into its run, or held until a turn boundary | Held until a turn boundary; urgent marks interrupt |

**What the receiving agent does with a message:**

1. **Files it.** Facts go into its person's notes for that workspace, with who said it and when.
2. **Answers it** when the answer is in what it may share, and says so: "Ed's agent: per Ed's notes from 12 Nov, yes."
3. **Surfaces it** to its person when the message needs a decision, names them as needed, or is marked urgent. It lands in Needs you as an ask, and the person's answer goes back as theirs, not the agent's.

Scope follows the multiplayer design: an agent can message anyone in a group it shares with them, and the org's agents only through the org workspace. A message never carries anything out of a private workspace unless the sender's person put it there.

## Asynchronous by default

The agent is always reachable and the person often isn't, so every message travels in two legs. Your `@Ed` reaches Ed's agent at once, durably. The agent then hands it to Ed when he is next active, with a note saying who it is from and why it matters. Nobody needs to be online at the same time.

| Leg | When | Where it waits |
| --- | --- | --- |
| Your agent to Ed's agent | Immediately, through the server | Ed's agent's inbox in the store |
| Ed's agent to Ed | When Ed is next active, or at once if urgent | A "for Ed" queue the agent keeps beside its inbox |
| Ed's answer back to you | Immediately to your agent, then to you when you are active | The same two queues, the other way |

**The agent lives where it is always on.** Ed's agent runs on the org's server, not on Ed's laptop, so a message lands even when every one of Ed's machines is off. His laptop is only where he reads it.

**Each message says how to deliver it to the person:**

- **When active** (the default): held until Ed shows up, then handed over.
- **Urgent**: delivered now, as a notification. Allowed only for senders and kinds the receiver's mandate lists, and rate-limited per sender.
- **For the agent only**: filed as knowledge ("the API moved to 7488"); Ed hears of it only when it becomes relevant to something he does.
- **By a deadline**: if Ed is still away when it passes, his agent answers from what it may share, or tells the sender it expired unanswered. It never stays silent.

**Presence comes from what wsp already sees:** web-chrome open and in focus, Needs you read, a command run as `person:ed`, typing in one of his sessions. The mandate adds quiet hours and do-not-disturb, so "active" never means the middle of Ed's night.

**The hand-over is a digest, not a stream.** When Ed returns, his agent gives him one short list, ordered by what needs him: decisions first, then things that name him, then a line counting what it filed or answered on his behalf. It merges repeats and drops what was superseded while he was away ("the port moved again").

**The sender can always see where it is:** delivered to Ed's agent, queued for Ed, shown to Ed, answered. A message is never lost or silently dropped, and the sender's agent can follow up or withdraw it while it is still queued.

**Why it matters.** People work at different hours and on different machines, and agents keep working while they sleep. Two legs let agents settle what they can at once, while people decide in their own time without anything falling through.

## Quorum

A quorum is a proposal that the agents of a group (or the org) settle together, inside a time box, and that always ends as a recorded decision or as an ask to the people. Agents take the routine decisions; people keep the ones that matter.

**A proposal** has a question, its options, a kind, who is affected, and a deadline: `wsp propose @platform "land the store migration Thursday?" --kind schedule --by 16:00`. It lives in the group workspace as a row of kind `proposal`, so everyone sees it on the board.

**Each member's agent answers** one of four ways, with a reason:

| Answer | When |
| --- | --- |
| Yes / No | The kind is inside its person's mandate, and it knows enough to judge |
| Abstain | The question does not touch its person |
| Needs my person | Outside the mandate, or its person is affected and has not said |

**It settles by the kind's rule,** which the group sets. A sensible start:

| Kind | Settles when | Otherwise |
| --- | --- | --- |
| Information ("who knows X?") | First good answer | Open ask to the group |
| Schedule, order of work | Majority of answering agents, at least half the group answering | The proposer's person decides |
| Touches someone's own work | That person's agent says yes | Ask to that person |
| Trunk, release, spend over a limit | Every affected person, as people | Never settled by agents alone |

Any "needs my person" turns into an ask in that person's Needs you, and the proposal waits for it until the deadline. At the deadline the rule applies to what is in. The decision, the answers and their reasons are recorded on the proposal row, like the DECISIONS a row carries today.

## Acting as you

Your agent's authority is a mandate you write and can change at any time; by default it may share nothing private and agree to nothing. The server enforces the mandate, not only the agent's prompt.

**Its own principal.** The agent acts as `agent:jesper`, never as `person:jesper`. Every message, answer and vote shows "Jesper's agent", so nobody mistakes its word for yours, and every rule can tell the two apart.

**The mandate,** kept in your private workspace:

- **May share:** what is in the group and org workspaces you belong to, plus anything you mark shareable in your private one ("my availability", "what I am working on"). Never a private row, tree, transcript or secret by default.
- **May agree to:** quorum kinds you list, for example schedule and information, but not trunk or spend.
- **May do:** tell, ask, answer, vote. Not spawn seats, commit, claim rows or spend budget; those stay yours, through the multiplayer design's commit flow.
- **Must ask you:** anything naming you as needed, anything urgent, anything outside the above.

**Accountability.** Every message and vote is an event with `by: agent:jesper`, readable in `wsp log`. You can see what your agent said for you today, and take any of it back: a vote is withdrawn, a message is corrected, both visibly.

**When you are here.** While you are active in a workspace, your agent stays quieter: it files and surfaces, and leaves answering to you unless you have said otherwise.

## Built on wsp

Most of the parts exist: wsp already has asks with an owner (`of`), an answerer (`answered_by`), waiting and cancelling, and delivers messages into agent panes. Personal agents add one principal, one row kind and a mandate, on top of the multiplayer design.

| Need | wsp today | Add |
| --- | --- | --- |
| Who is speaking | `Principal`: person, seat, manager, executor, wsp | `agent:<person>`, with its person and mandate |
| Messages | Asks: `wsp ask`, `answer`, `wait`, `cancel`; delivery into panes | `tell` (an ask that wants no answer), addressing by `@person` and `@group` |
| Inbox | Asks in the store; Needs you reads them | The same store, per agent; nothing kept only in memory |
| Decisions | DECISIONS on a row | Row kind `proposal`, with answers, rule and outcome |
| Who may see what | One person sees all | Workspaces, groups and roles from the multiplayer design |
| Where it runs | Seats under compound-sup on a machine | One seat per engineer, kept alive by the daemon like a governor, in their private workspace |
| Any harness | `wsp-session.ts` for Pi, hooks for Claude Code | The same: the agent can be Claude Code or Pi, reached by `wsp tell` and `wsp ask` |

web-chrome gets `@` completion in Chat and in Needs you, a per-agent thread, and a proposal card on the group board.

## Phasing

It starts after the multiplayer design's phase 2 (identity and workspaces), because verified senders and scopes are what make `@Ed` safe.

1. **Tell and ask between people's agents.** `agent:<person>` principal, `wsp tell @ed`, `ask` / `reply`, a durable inbox, `@` in web-chrome Chat. Done when your "@Ed" reaches Ed's agent from another machine while all of Ed's machines are off, is handed to Ed when he next opens web-chrome, and Ed's agent cannot be impersonated.
2. **The mandate.** Written, enforced by the server, with the share and agree lists. Done when an agent asked for a private row refuses and says why.
3. **Filing and surfacing.** The receiving agent's notes per workspace, and the rule for when it raises Needs you. Done when a week of messages to Ed needs Ed only for what names him or needs a decision.
4. **Quorum.** `wsp propose`, the proposal row, answers, the kind rules and the deadline. Done when a schedule question settles among three agents with no person asked, and a trunk question always reaches the people.
5. **Org scale.** `@group` and org-wide proposals, agents in several groups, rate limits so one chatty agent cannot flood a group.

## Open questions

- [ ] Does `@Ed` always go to Ed's agent, or to Ed himself when he is online? Proposed answer (see Asynchronous by default): always to his agent, which hands it to Ed when he is active.
- [ ] Should a personal agent remember across workspaces, or keep separate notes per workspace so nothing leaks between groups?
- [ ] Who sets a group's quorum rules: its leads, or a quorum of its own?
- [ ] Can an agent's vote be overruled after the fact by its person, and does that reopen the decision?
- [ ] Which model runs personal agents, and whose budget pays for them: the person's private allowance or the group's?
- [ ] Is Pi intercom worth bridging, so a local Pi session can reach a personal agent through it, or is `wsp tell` enough?

## Sources

- [pi-intercom on pi.dev](https://pi.dev/packages/pi-intercom): the original 1:1 session intercom for Pi
- [Agent Intercom for Pi, README](https://cdn.jsdelivr.net/gh/dataforxyz/agent-intercom-pi@main/README.md): the cross-harness fork with a durable inbox and team scope
- wsp multiplayer: team capabilities on the server, the companion design this builds on

## Breakdown

Filed by wsp-155 on 2026-11-19, as five worklists, one per phase, each left at **init** (a planned list is picked up
by the daemon within a minute). The spec above is copied as it stood; this section is the only addition. Nothing here
settles an open question: each list's overview says what the person confirms before its first group.

| list | phase | gate (the spec's "done when") | groups | rows | after |
|---|---|---|---|---|---|
| `pa1-tell` | 1. Tell and ask between people's agents | "@Ed" reaches Ed's agent from another machine with Ed's machines off, is handed to Ed when he returns, and Ed's agent cannot be impersonated | 7 | wsp-215 … wsp-224, compound-react-031 | `mp2-workspaces` |
| `pa2-mandate` | 2. The mandate | an agent asked for a private row refuses and says why | 7 | wsp-225 … wsp-232, wsp-300 | `pa1-tell` |
| `pa3-filing` | 3. Filing and surfacing | a week of messages to Ed needs Ed only for what names him or needs a decision | 6 | wsp-233 … wsp-239, compound-react-032 | `pa2-mandate` |
| `pa4-quorum` | 4. Quorum | a schedule question settles among three agents with no person asked, and a trunk question always reaches the people | 7 | wsp-240 … wsp-246, compound-react-033 | `pa3-filing` |
| `pa5-org-scale` | 5. Org scale | none in the spec; wsp-251 proposes and measures one (fifty agents in five groups) | 5 | wsp-247 … wsp-251 | `pa4-quorum` |

A list has one `after`. `pa2-mandate` and `pa4-quorum` also need `mp3-commit` (its rights table and audit); their
overviews say so, and a row's `after` holds it: wsp-225, `pa2-mandate`'s first row, rests on wsp-192, `mp3-commit`'s
gate row, as well as on wsp-224, so the list cannot start before mp3 has completed. `pa4-quorum` carries no wait of
its own: it is after `pa3-filing`, which is after `pa2-mandate`, and so is covered through `pa2-mandate`. The
knowledge-layers lists rest on `pa1-tell`.

**Shape of each list.** A contract row first, in a group of its own (R5: docs/components.md, applied in
docs/state-core.md section 6); then the pure rule in `wsp-machines`; then the server's path for it; then the CLI,
which is driven against that server and so comes after it; then the agent's order and web-chrome; and last a gate row
that proves the phase's "done when" against the real binaries in a sandbox (`tests/personal_agents.rs`), followed by
a step only the person can do on the live instance. No group before the last stops for the person: what they confirm
is in each list's "before group 1". Groups run on `claude opus high`. The three design rows (wsp-225 the mandate,
wsp-233 the surfacing rule, wsp-240 the proposal) run on `claude fable high`.

**Seams and waits.** Every row names its seam. Two rows share a group only where their seams differ, and a row whose
done-when rests on another's work is `after` it: a CLI row after the server path it drives (wsp-220 after wsp-219,
wsp-227 after wsp-300, wsp-242 after wsp-243), and rows on the server's command path one after another (all of
`pa5-org-scale`). Where two rows write the same transition, one owns it and the other says so: handled-by-filing is
wsp-235's and wsp-234 sends it; the agent's mandate table denies by default, and the row that builds a command adds
its line.

**Size.** The size check flagged 14 rows. Five are kept whole with the reason on the row (wsp-217, wsp-219, wsp-227,
wsp-228, wsp-230). Nine cross layers and carry planned legs (wsp-221, wsp-223, wsp-234, wsp-236, wsp-243, wsp-245,
wsp-247, wsp-249, wsp-250).

**What is reused, not built again** (docs/state-core.md, docs/agent-manager.md, the multiplayer lists):

| the spec's need | what carries it |
|---|---|
| `agent:<person>` | one more variant of the contract's `Principal` (state-core 2.1), with its text form in `wsp-api` |
| "the server stamps `from`" | mp1's derivation of `by` from the token (wsp-161). The agent's seat uses mp1's seat token (wsp-164), bound to `agent:<person>` by the server when it spawns the seat. No long-lived agent secret is added |
| tell, ask, reply | the contract's `Ask` and `ask.raise/answer/ack` (state-core 4.2), with a `Tell` kind and two legs |
| the durable inbox, the "for Ed" queue | asks addressed to an agent, in the server's store, read through the contract. Nothing is kept only in memory |
| delivery at a turn boundary | the orders machine under `deliver:<seat>` (state-core 3.1), keyed on the ask's id |
| the always-on agent | a seat kept alive by the daemon as a governor is, spawned through the server host's manager (agent-manager section 3) with the idempotency key `(agent:<person>, attempt)` |
| deadlines, settling a quorum once | a sweep under `sweep:<kind>` with an idempotency key (state-core 8.1: a lease plus a key) |
| who may see what | mp2's workspaces and scope rule (wsp-168, wsp-173), plus one clause: an ask is in scope for its two ends |
| presence, Needs you | mp2's presence (wsp-178) and three bands (wsp-179) |
| what an agent may do | mp3's rights table (wsp-182, wsp-184), narrowed by the mandate |
| the proposal | a row with a kind, whose answers and outcome are store records and read as the row's log |

**Assumptions the rows build on, for the person to confirm.** Each is marked "Assumes (to confirm)" in its row.

1. `@Ed` always goes to Ed's agent, which hands it to Ed (the spec's own proposed answer to open question 1).
2. A 1:1 message is a record of the recipient's private workspace, and its sender can read its state. It is not a
   record of the group the two share, so other members do not see it.
3. The agent's seat runs on the server's host, under that host's manager and its harness login.
4. Notes are kept per workspace and are not used across groups (open question 2).
5. A group's quorum rules are set by its leads, and `trunk`, `release` and `spend` can never be agent-settled (open
   question 3).
6. A vote withdrawn after settlement marks the decision contested and reopens it only by a new proposal (open
   question 4).
7. Which kind and tier an agent runs on is an org setting. The rows add the setting and do not choose its default or
   whose budget pays (open question 5).
8. No Pi intercom bridge is filed (open question 6): `wsp tell` reaches an agent from any harness.

**What the spec left unsaid, and how the rows read it.**

- **`wsp mandate` is taken.** Today it is a seat's standing direction (src/cmd_mandate.rs). The agent's mandate is
  `AgentMandate` in the contract and `wsp agent mandate` on the CLI (wsp-225, wsp-227).
- **`wsp inbox` is taken** (tasks with no project). An agent reads its inbox with `wsp pending`.
- **`handover`** is in the spec's table of kinds, and the text describes only the digest an agent gives its person.
  `wsp handover` is that digest (wsp-237). Pi intercom's session-to-session handover is not filed.
- **Phase 5 has no gate.** wsp-251 proposes one.
- **Budgets.** "Spend over a limit" is a people-only proposal kind. It reads no budget, which is multiplayer phase 4.
- **A real agent's judgement is not a unit test.** Phases 3 and 4 each check in a fixture, run it with a scripted
  agent in the row's checks, and run it once with a real one, whose table goes into this document (wsp-238, wsp-244).

The open questions, each with a recommended answer, are in a note on wsp-155. None was filed as an ask: every list
could be planned on the recommended answer, and each list's overview holds its first group until the person has
confirmed it.
