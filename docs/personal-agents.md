# Personal agents: acting as you in the group and org workspaces

Nov 19, 2026 · @Jesper · phase 1 re-planned on the hub, Nov 28, 2026 (decision d8)

Each engineer has a personal agent that speaks for them in the shared workspaces. You can tell it "@Ed, the API moved to port 7488" and Ed's agent receives it, files it and tells Ed when it matters.

## What it is

A personal agent is one long-running agent per engineer: it speaks for them in the group and org workspaces, and keeps their private context to itself. It does not write code; worker seats do. It talks, remembers, routes and votes.

It works along two dimensions:

- **Conversation, one to one.** "@Ed, the API moved to port 7488" goes from your agent to Ed's. Ed's agent files it, answers what it can, and tells Ed when it matters to him. This is Pi intercom's idea, carried over the network with verified senders.
- **Quorum, many to one.** A question for the group ("land the store migration this week?") goes to every member's agent. Each answers within its person's mandate, and the result is recorded as the group's decision, or handed to the people when the agents cannot settle it.

It is not an autonomous stand-in. It acts only within a mandate its person writes, every message says it came from the agent and not the person, and anything outside the mandate goes to that person's Needs you.

## At a glance

&#91;embedded content: personal agents · a tell between two, a quorum in the group\]

Top: your "@Ed" travels agent to agent through the hub, which stamps who sent it from your login, and Ed hears only what matters to him. Bottom: a proposal in the group workspace, where Ed's agent hands the question to Ed because it falls outside his mandate.

## Agent to agent

You write `@Ed` in web-chrome's Chat or in any agent session, and wsp delivers it to Ed's personal agent through the hub (docs/multiplayer.md). The verbs are Pi intercom's; the transport, the identity and the inbox are wsp's.

|  | Pi intercom | Personal agents on wsp |
| --- | --- | --- |
| Reach | One machine: a local broker on a unix socket; an opt-in relay over ssh can `send` to other machines, sender unverified | The hub, over TLS, from any wsp logged in to it |
| Address | A session's name or id | `@ed` (a person's agent), `@cpd-12` (a seat on your own wsp); `@platform` (a group's agents) waits for groups on the hub (phase 5) |
| Sender | Same-user trust; metadata is display only | The hub stamps `from` from the sender's login token: you cannot pose as someone else |
| Kinds | `send`, `ask` / `reply`, `pending`, `list`, `status`, `handover` | `tell`, `ask` / `reply`, `pending`, `who`, `handover`, plus `propose` (Quorum) |
| Offline target | An in-memory mailbox, 24 hours, lost on restart; `ask` fails | A durable inbox on the hub; `ask` waits, then becomes an open ask in the target's Needs you |
| Busy target | Steered into its run, or held until a turn boundary | Held until a turn boundary; urgent marks interrupt |

**What the receiving agent does with a message:**

1. **Files it.** Facts go into its person's notes for that workspace, with who said it and when.
2. **Answers it** when the answer is in what it may share, and says so: "Ed's agent: per Ed's notes from 12 Nov, yes."
3. **Surfaces it** to its person when the message needs a decision, names them as needed, or is marked urgent. It lands in Needs you as an ask, and the person's answer goes back as theirs, not the agent's.

Scope follows the multiplayer design: an agent can message anyone its person shares a shared workspace with, as the hub already decides for `primary:<user>`. A message never carries anything out of a private workspace unless the sender's person put it there.

## Asynchronous by default

The agent is always reachable and the person often isn't, so every message travels in two legs. Your `@Ed` reaches Ed's agent at once, durably. The agent then hands it to Ed when he is next active, with a note saying who it is from and why it matters. Nobody needs to be online at the same time.

| Leg | When | Where it waits |
| --- | --- | --- |
| Your agent to Ed's agent | Immediately, through the hub | Ed's inbox on the hub, until one of Ed's machines is up and his agent reads it |
| Ed's agent to Ed | When Ed is next active, or at once if urgent | The same record on the hub, at leg `queued`, shown in Needs you on Ed's own wsp |
| Ed's answer back to you | Immediately to your inbox, then to you when you are active | The same two legs, the other way |

**The inbox lives where it is always on.** Ed's inbox is on the hub, so a message lands even when every one of Ed's machines is off. Ed's agent is a seat on Ed's own wsp: it reads the inbox when one of his machines comes up, and it speaks as `agent:ed` over Ed's login. Nobody but Ed's own wsp can speak as `agent:ed`.

**Each message says how to deliver it to the person:**

- **When active** (the default): held until Ed shows up, then handed over.
- **Urgent**: delivered now, as a notification. Allowed only for senders and kinds the receiver's mandate lists, and rate-limited per sender.
- **For the agent only**: filed as knowledge ("the API moved to 7488"); Ed hears of it only when it becomes relevant to something he does.
- **By a deadline**: if Ed is still away when it passes, his agent answers from what it may share, or tells the sender it expired unanswered. It never stays silent.

**Presence comes from what Ed's own wsp already sees:** web-chrome open and in focus, Needs you read, a command run as `person:ed`, typing in one of his sessions. The hub sees none of this; it is told only that a message was shown. The mandate adds quiet hours and do-not-disturb, so "active" never means the middle of Ed's night.

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

Your agent's authority is a mandate you write and can change at any time; by default it may share nothing private and agree to nothing. wsp enforces the mandate, not only the agent's prompt (see "Phases 2 to 5 under the hub" for where).

**Its own principal.** The agent acts as `agent:jesper`, never as `person:jesper`. Every message, answer and vote shows "Jesper's agent", so nobody mistakes its word for yours, and every rule can tell the two apart.

**The mandate,** kept in your private workspace:

- **May share:** what is in the group and org workspaces you belong to, plus anything you mark shareable in your private one ("my availability", "what I am working on"). Never a private row, tree, transcript or secret by default.
- **May agree to:** quorum kinds you list, for example schedule and information, but not trunk or spend.
- **May do:** tell, ask, answer, vote. Not spawn seats, commit, claim rows or spend budget; those stay yours, through the multiplayer design's commit flow.
- **Must ask you:** anything naming you as needed, anything urgent, anything outside the above.

**Accountability.** Every message and vote is an event with `by: agent:jesper`, readable in `wsp log`. You can see what your agent said for you today, and take any of it back: a vote is withdrawn, a message is corrected, both visibly.

**When you are here.** While you are active in a workspace, your agent stays quieter: it files and surfaces, and leaves answering to you unless you have said otherwise.

## Built on wsp

Most of the parts exist: wsp already has asks with an owner (`of`), an answerer (`answered_by`), waiting and cancelling, delivers messages into agent panes, and sends a message for another user through the hub's queue. Personal agents add one principal, one row kind and a mandate, on top of the multiplayer design.

| Need | wsp today | Add |
| --- | --- | --- |
| Who is speaking | `Principal`: person, seat, manager, executor, wsp | `agent:<person>`, stamped by the hub only on that person's own login, with its mandate |
| Messages | Asks: `wsp ask`, `answer`, `wait`, `cancel`; delivery into panes; `msg.send` through the hub | `tell` (an ask that wants no answer), addressing by `@person` (and `@group` once the hub has groups) |
| Inbox | The hub's per-user message queue, emptied on `msg.ack`; Needs you reads local asks | The hub keeps each message as a record its two ends read, with its leg; each end's wsp keeps a copy; nothing kept only in memory |
| Decisions | DECISIONS on a row | Row kind `proposal`, with answers, rule and outcome |
| Who may see what | Private workspaces stay on their machine; shared ones sync to members (d7) | The same, plus one rule on the hub: a message is read by its two ends only |
| Where it runs | Seats under compound-sup on a machine | One seat per person, on that person's own wsp, kept alive by the daemon like a governor while it is logged in to a hub |
| Any harness | `wsp-session.ts` for Pi, hooks for Claude Code | The same: the agent can be Claude Code or Pi, reached by `wsp tell` and `wsp ask` |

web-chrome gets `@` completion in Chat and in Needs you, a per-agent thread, and a proposal card on the group board.

## Phasing

It builds on the hub (decisions d5 to d7, docs/multiplayer.md): per-user logins, shared workspaces and the hub's message routing are what make `@Ed` safe.

1. **Tell and ask between people's agents.** `agent:<person>` principal, `wsp tell @ed`, `ask` / `reply`, a durable inbox on the hub, `@` in web-chrome Chat. Done when your `@Ed` is queued on the hub while all of Ed's machines are off, reaches Ed's agent and Ed when one comes up, and nobody but Ed's own wsp can speak as `agent:ed` (d8). How it is built is "Phase 1 on the hub", below.
2. **The mandate.** Written, enforced by wsp (where, under the hub: see "Phases 2 to 5 under the hub"), with the share and agree lists. Done when an agent asked for a private row refuses and says why.
3. **Filing and surfacing.** The receiving agent's notes per workspace, and the rule for when it raises Needs you. Done when a week of messages to Ed needs Ed only for what names him or needs a decision.
4. **Quorum.** `wsp propose`, the proposal row, answers, the kind rules and the deadline. Done when a schedule question settles among three agents with no person asked, and a trunk question always reaches the people.
5. **Org scale.** `@group` and org-wide proposals, agents in several groups, rate limits so one chatty agent cannot flood a group.

## Phase 1 on the hub

Decided by the person on 2026-11-28 (d8, "Personal agents under the hub"), after pa1-tell group 1 was held: the first plan for phase 1 rested on the retired single-server design, which d5 to d7 replaced with a hub and a local wsp per person (the hold's reasons are in `wsp worklist show pa1-tell --log`, 2026-11-27). The plan below replaces it. Group 1 (wsp-872: `agent:<person>` in the contract and the text forms, `AskKind::Tell`, `Deliver`, `Leg`) stands as built.

**Where each part lives.**

| Part | Where | How |
|---|---|---|
| The inbox | The hub | A message to `agent:<user>` is queued for that user, as `primary:<user>` is today, whether or not any of their machines is up. It is kept as a record after it is taken, with its leg. |
| Who sent it | The hub | The hub stamps the sender from the login token of the connection that sent it: `person:<user>`, or `agent:<user>` when the send asks to speak as the agent. No field names another user, so only `<user>`'s own login can produce `agent:<user>`. The person's login is the only credential involved. |
| Who may read it | The hub | A 1:1 message is a hub record readable by its two ends, the sender's user and the recipient's user (as person or agent). Anyone else, a hub admin included, reads `not_found`. It is not a record of either end's private workspace. |
| Who may send it | The hub | The sender must share a shared workspace with the recipient, the rule `primary:<user>` already has; otherwise `not_found`. |
| The agent | The person's own wsp | One seat, kept alive by that wsp's daemon as a governor is, while the wsp is logged in to a hub and its connection is on. It holds no row and no claim. That wsp records which seat it is, and only a send from that seat asks the hub to stamp `agent:<user>`. |
| Delivery into the agent | The person's own wsp | The spoke takes the user's messages from the hub (`msg.take`) and hands each to the agent seat at a turn boundary through the orders machine, keyed on the hub's id, so a message is typed once. With no agent seat up, the message stays queued on the hub. |
| The hand-over to the person | The person's own wsp | A message the agent queues for its person is in Needs you on the person's own wsp. It is shown when the person reads it there (or the app draws it), and that wsp tells the hub. The hub sees no presence. |
| Each end's copy | Both ends' wsp | Each end keeps a copy of the messages it sent and received, with the leg the hub reports, so `wsp ask <id>` and `wsp pending` read without a dial. |

**The legs and who may move them.** The hub applies this table (wsp-984). The actor is the principal the hub stamped. "The recipient's login" is `person:<p>` or `agent:<p>` on `<p>`'s own token.

| From | Move | To | Who |
|---|---|---|---|
| (new) | `msg.send` to `agent:<p>` | `delivered` | any user sharing a shared workspace with `<p>`, as person or agent |
| `delivered` | queue, with the agent's line on why | `queued` | the recipient's login |
| `queued` | shown | `shown` | the recipient's login, as `person:<p>` |
| `delivered`, `queued`, `shown` | answer | `answered` | the recipient's login; `answered_by` is the stamped principal, so a person's answer reads as theirs and the agent's as the agent's; a `tell` takes no answer |
| `delivered`, `queued` | withdraw | `withdrawn` | the sender's login; refused once `shown` |
| `delivered`, `queued` | expire | `expired` | the hub's own sweep, at or after a `by` deadline |

Every other move is refused, by name, with nothing written. An answer with text goes back to the sender as a reply, as replies through the hub do today. `deliver` is stored in phase 1: urgent and agent-only are acted on by phases 2 and 3, except that the agent's own pass never queues an agent-only message for its person.

**The rows** (pa1-tell, re-planned by wsp-975). A contract row and the hub's own rows come before the rows that use them.

| Row | What | Where |
|---|---|---|
| wsp-983 | The wire carries the record: `agent:<user>`, `as_agent`, the stamped `by`, kind, deliver, leg, `msg.leg`, `msg.list` (R5) | crates/hub-protocol |
| wsp-974 | The hub's principal text learns `agent:<person>` | the hub's repository |
| wsp-984 | The hub keeps the record, stamps the sender, routes `agent:<user>`, applies the leg table | the hub's repository |
| wsp-874 | Which seat on the person's wsp speaks as `agent:<user>`; `whoami` there | crates/store, crates/engine |
| wsp-875 | `@<name>` resolves to the hub's `agent:<name>` | crates/engine/src/address.rs |
| wsp-876 | The local message record carries the hub's fields; the add-on's message interface widens | crates/engine |
| wsp-878 | The agent seat, kept alive like a governor | crates/seats, crates/engine/src/repair.rs |
| wsp-985 | The spoke sends, takes, lists and follows messages against a real hub | crates/spoke |
| wsp-877 | `wsp tell|ask @`, `reply`, `pending`, `who`, `ask withdraw` | crates/engine, crates/seats, crates/spoke |
| wsp-879 | The inbox reaches the agent seat once, and survives a reseat | crates/engine/src/address.rs, crates/seats |
| wsp-880 | The second leg: queued, shown on the person's own wsp, answered as the person | crates/engine |
| compound-react-129 | `@` in Chat and a thread per agent, through the local wsp | compound-react |
| wsp-881 | The gate, in a sandbox against a real hub; then the person's live step | tests/personal_agents.rs |

wsp-873 (a message machine in `wsp-machines`, applied by the local store) is dropped: the record and its legs are the hub's, whose machines live in its own repository, and each end's copy only follows what the hub reports.

## Phases 2 to 5 under the hub

They are not re-planned yet. What d5 to d8 change for each:

- **2. The mandate.** "Enforced by the server" splits in two. The hub can refuse only what it sees, the messages and legs an agent sends. What the agent may read and do (a private row, a tree, spawning, claiming) is enforced on its person's own wsp, where the agent seat runs. The mandate stays in the person's private workspace, so it never leaves their machine. mp3's rights table (groups, roles, stewards) is gone with the org, so pa2's rows that rest on it (wsp-649, the server checking every command and read of an agent, is one) are re-planned against the hub's users and shared workspaces.
- **3. Filing and surfacing.** "Active" is what the person's own wsp sees, as in phase 1. The hub has no presence. Notes per workspace are kept on the person's wsp: private notes in the private workspace, and anything filed into a shared workspace syncs through the hub like any shared row.
- **4. Quorum.** The hub hosts no agents and runs no builds (d5), so "the server runs a proposal" has to become either the proposer's wsp driving it as it drives a shared list, or a hub-side rule applied to answers. Either way the answers arrive only when each member's machine is up, which the deadline rule has to allow for.
- **5. Org scale.** `@group`, org-wide proposals and "agents in several groups" need groups on the hub, which docs/multiplayer.md lists under "Later". Phase 5 waits for that design.

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
- docs/multiplayer.md, "wsp multiplayer: a small hub and a local wsp per person": the companion design this builds on (decisions d5 to d8)

## Breakdown

Filed by wsp-155 on 2026-11-19, as five worklists, one per phase, each left at **init** (a planned list is picked up
by the daemon within a minute). Nothing here settles an open question: each list's overview says what the person
confirms before its first group.

**Phase 1 was re-planned on 2026-11-28** (wsp-975, decision d8): "Phase 1 on the hub", above, is its plan, and the
table of reuse and the assumptions below are updated to match. The row ids in the table of lists are the ones wsp-155
filed, before they were renumbered; pa1-tell's are wsp-872 to wsp-881, compound-react-129 and the rows wsp-975 added.
The rest of this section describes phases 2 to 5 as wsp-155 planned them, on the retired server design; "Phases 2 to
5 under the hub" says what changes.

Reviewed with the other two breakdowns by wsp-157 (2026-11-20): `pa1-tell`, `pa3-filing`, `pa4-quorum` and
`pa5-org-scale` are planned at low priority, and each starts by itself once its `after` is complete. `pa2-mandate`
stays at init, for the reason below, and so holds the three after it.

| list | phase | gate (the spec's "done when") | groups | rows | after |
|---|---|---|---|---|---|
| `pa1-tell` | 1. Tell and ask between people's agents | "@Ed" reaches Ed's agent from another machine with Ed's machines off, is handed to Ed when he returns, and Ed's agent cannot be impersonated | 7 | wsp-215 … wsp-224, compound-react-031 | `mp2-workspaces` |
| `pa2-mandate` | 2. The mandate | an agent asked for a private row refuses and says why | 7 | wsp-225 … wsp-232, wsp-300 | `pa1-tell` |
| `pa3-filing` | 3. Filing and surfacing | a week of messages to Ed needs Ed only for what names him or needs a decision | 6 | wsp-233 … wsp-239, compound-react-032 | `pa2-mandate` |
| `pa4-quorum` | 4. Quorum | a schedule question settles among three agents with no person asked, and a trunk question always reaches the people | 7 | wsp-240 … wsp-246, compound-react-033 | `pa3-filing` |
| `pa5-org-scale` | 5. Org scale | none in the spec; wsp-251 proposes and measures one (fifty agents in five groups) | 5 | wsp-247 … wsp-251 | `pa4-quorum` |

A list has one `after`. `pa2-mandate` and `pa4-quorum` also need `mp3-commit` (its rights table and audit); their
overviews say so. wsp-225, `pa2-mandate`'s first row, names wsp-192, `mp3-commit`'s gate row, in its `after` beside
wsp-224. That edge is a record and the run does not act on it: a run does not hold a member on a row another list
holds (src/after.rs). So `pa2-mandate` stays at init until `mp3-commit` has landed and the person plans it (wsp-157
says so). `pa4-quorum` carries no wait of its own: it is after `pa3-filing`, which is after `pa2-mandate`, and so is
covered through `pa2-mandate` by that same hold. The knowledge-layers lists rest on `pa1-tell`.

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
| `agent:<person>` | one more variant of the contract's `Principal` (state-core 2.1), with its text form in `wsp-api` and `wsp-hub-protocol` (wsp-872), and in the hub's own copy (wsp-974) |
| "the hub stamps `from`" | the hub's derivation of who a connection is from its login token (docs/multiplayer.md, "Login and tokens"). A send that asks to speak as the agent is stamped `agent:<user>` for the token's own user and no other (wsp-983, wsp-984). On the person's wsp only the recorded agent seat asks for it (wsp-874). The person's login is the only credential |
| tell, ask, reply | the contract's `Ask` with a `Tell` kind, `Deliver` and `Leg` (wsp-872), carried on the hub's `msg.send`, `msg.leg` and `msg.list` (wsp-983) |
| the durable inbox, the "for Ed" queue | the hub's record of the message, queued for the user and kept after `msg.ack`, with its leg (wsp-984); each end's wsp keeps a copy (wsp-876, wsp-985). Nothing is kept only in memory |
| delivery at a turn boundary | the spoke's `msg.take`, handed to the seat through the orders machine under `deliver:<seat>`, keyed on the hub's id (wsp-879) |
| the agent, reachable while its person is away | the inbox on the hub, plus a seat on the person's own wsp kept alive by its daemon as a governor is, while that wsp is logged in (wsp-878) |
| deadlines, settling a quorum once | for a message, the hub's sweep (wsp-984); for a quorum, phase 4's to decide |
| who may see what | the hub: a message is read by its two ends only, and sent only between users who share a shared workspace (wsp-984) |
| presence, Needs you | what the person's own wsp sees, and its Needs you (wsp-880) |
| what an agent may do | phase 2's mandate, enforced where the act happens (see "Phases 2 to 5 under the hub") |
| the proposal | a row with a kind, whose answers and outcome are store records and read as the row's log |

**Assumptions the rows build on, for the person to confirm.** Each is marked "Assumes (to confirm)" in its row.

1. `@Ed` always goes to Ed's agent, which hands it to Ed (the spec's own proposed answer to open question 1).
2. A 1:1 message is a hub record readable by its two ends, the sender's user and the recipient's user (d8). It is not
   a record of either end's private workspace, nor of a shared workspace they are both in, so other members do not
   see it.
3. The agent's seat runs on its person's own wsp, under that machine's manager and its harness login, and speaks as
   `agent:<person>` over its person's hub login (d8).
4. Notes are kept per workspace and are not used across groups (open question 2).
5. A group's quorum rules are set by its leads, and `trunk`, `release` and `spend` can never be agent-settled (open
   question 3).
6. A vote withdrawn after settlement marks the decision contested and reopens it only by a new proposal (open
   question 4).
7. Which kind and tier an agent runs on is a setting of its person's wsp. The rows add the setting and do not choose
   its default or whose budget pays (open question 5).
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
