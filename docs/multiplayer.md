# wsp multiplayer: team capabilities on the server

Nov 19, 2026 · @Jesper · Draft

When this is built, three or more engineers run compound against one wsp server. Each has a private workspace, and each can commit agents into their group's shared workspace. The org holds groups, and each group holds engineers, so the same design carries from three people to many groups.

## Why

Today one wsp server serves exactly one person, and every rule says "the person". Multiplayer means an org of engineers sharing one control plane without sharing everything:

- **Private by default.** Each engineer experiments in their own workspace: their rows, worklists, agents and asks, seen by nobody else.
- **Group by commitment.** When something is worth sharing, the engineer commits it, an agent, a row or a worklist, to the group workspace. From then on the group sees it, owns it and pays for it.
- **One board per group.** Everyone in a group sees the same group rows, worklists, barriers and agents, live, and who did what.
- **Own Needs you.** An ask lands with the person it is for, or with the group when it is the group's; never in everyone's.

It is not one server per team. state-core's D4 says "multi-tenancy is by instancing": an org is the tenant and gets one instance. Groups and workspaces are scopes inside it.

## Where we are

The protocol is already ready for many people; the credentials and the rules are not. state-core 4.1 made `by` mandatory on every request so that "multi-person work is an auth and permissions change, not a protocol change". What still assumes one player:

| Piece | Today | Single-player assumption |
| --- | --- | --- |
| `api.token` | 32 random bytes per instance, mode 0600 | Whoever holds the file is anyone. `hello` names its own `by`, and the server only checks later requests against that hello. |
| `Principal::Person` | `person:ed`, or `$USER` in web-chrome's dial | Any token holder can claim to be any person. |
| Peer rule | Loopback plus the tailnet ranges (100.64.0.0/10, fd7a:115c:a1e0::/48) | Reaching the Linux host over the LAN means an ssh tunnel; leaving the office drops it. |
| State rules | "the person" may end a seat, `despawn --lost`, release a held group | One person holds every power. |
| Asks | An ask has `of` and `answered_by` | Already per-person in the record; Needs you shows them all to whoever looks. |
| Managers | One token per host (D13 leaves identity for later) | Fine for machines; people were deferred to state-core step 6. |

So the work is state-core step 6 ("auth and principals"), carried far enough for an org of groups and engineers, plus workspaces, which no step has yet.

## Org, groups and workspaces

A workspace is the unit of sharing: everything wsp holds today (projects, rows, worklists, seats, asks, holds) lives in exactly one workspace. Who can see a workspace follows from the org's groups.

&#91;embedded content: org · groups · private and group workspaces\]

Engineer A commits seat cpd-12 from their private workspace: it keeps running, now on the group's board and budget. B's and C's work stays private until they commit it.

| Level | Holds | Who sees it |
| --- | --- | --- |
| Org | Groups, engineers, machines, the org's budget; one wsp instance | Org admins see every group workspace and every private workspace's spend, never a private workspace's content |
| Group | Engineers (an engineer can be in several groups), one group workspace, a share of machines and budget | Its members; its leads manage membership and budget |
| Engineer | One private workspace, their tokens | Themselves |
| Workspace | Projects, rows, worklists, seats, asks, holds | Private: its engineer. Group: the group's members |

**Committing to the group.** An engineer commits an agent with `wsp commit <seat> --to <group>`; a row or a whole worklist commits the same way. A commit is one event: the item moves to the group workspace, the group becomes its owner, the engineer stays its steward, and from then on it spends the group's budget. A running seat keeps running; its pane, tree and claim move with it. The project must exist in the group workspace first, so a private experiment on a repo the group does not have cannot leak in by accident.

**After a commit.** The group can see, answer and stop the agent. Its asks go to the steward first and to the group when the steward is away. The steward or a group lead can recall it to a private workspace, which is also recorded. Nothing moves from group to private, or between groups, without such an event.

**Projects.** The same repo can be a project in several workspaces. Each workspace's seats work in their own worktrees, so a private experiment and the group's work on one repo never share a tree. Landing on the trunk stays the group's barrier rules.

## How it fits together

&#91;embedded content: multiplayer wsp · people, the shared server, seats\]

Auth is the one new box: it turns a person's token into their principal and role, and every rule after it reads that instead of trusting the hello. The store, the event stream and the manager stay as they are; every record gains its workspace, and every seat its steward.

## Identity and access

Each engineer gets their own credential, and the server derives `by` from it instead of trusting the hello. A request naming someone else is refused `forbidden`, as a mismatched `by` is today, and a request into a workspace the engineer cannot see is refused the same way.

**Credentials.** The org keeps `engineers`, `groups` and `members` (engineer, group, role). An admin adds an engineer with `wsp engineer add <name> --group <group>`, which prints a token once; later this can come from the org's single sign-on instead. The token is stored hashed. The instance token stays, for machines only (managers, the daemon). A revoked token is refused at the next hello and its open connections are dropped.

**Seats.** A seat acts as `seat:cpd-N` with a token its executor hands it, as state-core 4.1 plans. It records its workspace and its steward, the engineer it works for, so its rights are its workspace's rights and never more.

**Roles.** Roles are per group, plus one org role:

| Role | Scope | Can |
| --- | --- | --- |
| Org admin | Org | Groups, engineers, machines, budgets, install, the daemon; read every group workspace |
| Group lead | One group | Members and budget of the group; release any hold, end or recall any agent in the group workspace |
| Engineer | One group | Read and work the group workspace: add and claim rows, answer the group's asks, commit to it, end agents they steward |
| Watcher | One group | Read the group workspace and subscribe |

In a private workspace its engineer holds every right. The rules in state-core's transition tables change from "the person" to "the steward, or a group lead in a group workspace". Taking over another engineer's row or ask is allowed in a group workspace and always recorded.

## Ownership and attention

Every row, worklist, seat and ask has a workspace and a steward, and Needs you gathers what is the engineer's own across all their workspaces. This is where multiplayer is felt.

| Thing | Workspace | Steward | Moves by |
| --- | --- | --- | --- |
| Row | Where it was added | Who added or claimed it | `wsp commit`, `wsp give <row> <engineer>`, or a recorded take-over |
| Worklist | Where it was planned | Who planned it | `wsp commit`; its rows and seats move with it |
| Seat | Its row's or list's | Whoever spawned it; a governor's is the list's steward | `wsp commit <seat>`, `wsp recall <seat>` |
| Ask | Its seat's | Its `of`, already in the record | `wsp ask pass <id> <engineer>` |
| Hold | Its list's | Who put it on | Released by its steward, or a group lead |

**Needs you, for one engineer, in three bands:**

1. **Mine:** asks `of` me in any of my workspaces, my held lists at their stop, my rows finished and waiting for Mark done.
2. **My groups:** asks of a group I am in that nobody has taken, and anything whose steward has been away past a set time. Taking one makes it mine.
3. **Watching:** a group's whole attention list, for leads, and for looking over a colleague's shoulder.

A take-over in a group workspace never happens silently: answering another engineer's ask, or claiming their row, asks for confirmation, records `taken from <engineer>` and tells the previous steward.

## Presence and live state

The server already streams every event to every subscriber in one order, so all clients see the same board. Multiplayer adds who is here and what they are looking at, and keeps two people from writing over each other.

**Presence.** A connection's hello names its person, and the existing beat keeps it alive. The server keeps a small, unstored presence map: person, clients, last beat, and an optional `looking_at` (a row, list or seat) the client sends when its selection changes. web-chrome shows faces in the top bar and a dot on a row someone else has open. Presence is not written to the event log; it is gone when the connection is. It is scoped like everything else: a group's members see each other in the group workspace, and nobody sees what an engineer has open in their private one.

**Away.** A person with no connection for a set time, 30 minutes by default, counts as away. Their unanswered asks show under Team, and seats asking them can fall back to the list's owner.

**Concurrent edits.** Text sections already save against a base (`SectionEditor` sends `base`), so the second writer gets a conflict instead of a silent overwrite. The same rule goes on every command that changes something another person owns: it carries the version it saw and is refused `stale` if that moved. Claims already have fencing tokens; they stay as they are.

**Feed.** Each event records its `by`, so web-chrome can say "Sam committed cpd-12 to platform" in the group's feed and leave the person's own actions quiet.

## Reaching the server

Put the server and every player on one tailnet; the server's peer rule already admits the tailnet ranges, so no protocol change is needed. Today each Mac reaches the Linux host through an ssh tunnel to its LAN address, which only works on the office network and drops whenever the laptop sleeps or leaves.

| Option | Works off the LAN | Per-person identity | Change to wsp |
| --- | --- | --- | --- |
| ssh tunnel (today) | No, unless the LAN is exposed | ssh key, not seen by wsp | None |
| Tailscale or another WireGuard mesh | Yes | Tailnet node, plus the wsp token | None: the ranges are already allowed |
| Public TLS endpoint | Yes | wsp token only | A TLS listener (wsp-461, below); rate limits and a wider attack surface remain |

Recommended: Tailscale, with tailnet ACLs limiting port 7488 to the team's devices, and the per-person wsp token on top. web-chrome's `--target` then dials the target's tailnet name directly instead of the local `api.json`, which also fixes compound-react-014.

### The hub over TLS (wsp-461)

The hub (`wsp-hub serve --config <file>`) serves TLS when its config names a certificate, and a hub on a public endpoint must: it refuses to start bound to an address that is not loopback without one. No host is written in the code or the tests; the hub is whatever the config says, and a tailnet stays optional.

```json
{
  "data_dir": "/var/lib/wsp-hub",
  "bind": ["0.0.0.0"],
  "port": 7489,
  "peers": ["0.0.0.0/0", "::/0"],
  "tls": { "cert": "/etc/wsp-hub/fullchain.pem", "key": "/etc/wsp-hub/privkey.pem" }
}
```

- `tls.cert` is the PEM certificate chain and `tls.key` its private key (relative paths are read from the config's directory). With `tls` every connection is TLS, loopback included.
- `peers` is the hub's own list of CIDR blocks it admits; a public hub names `0.0.0.0/0` and `::/0`. It is the hub's alone: the daemon's server still admits loopback and the tailnet and nothing else, and sets no TLS.

**The URL.** A client names a hub by `tls://host:port` for TLS, which is the form for any hub that is not on loopback or a private network. `tcp://host:port`, or a bare `host:port`, is plain TCP, as before: the user's token crosses it in the clear. Any other scheme (`https://` too: the protocol is not HTTP) is refused, and a port is required.

**Trust.** A `tls://` hub's certificate must be for the URL's host and chain to the platform's root certificates. A private CA (a test, a self-hosted hub) is *pinned* with a PEM file, named by `--ca <file>`, `$WSP_HUB_CA`, or `"ca"` in `~/.config/wsp/hubs.json`: the client then trusts that CA's certificates and no other root. A certificate it refuses ends the dial as `Silent::Tls`, naming the fault (`invalid peer certificate: UnknownIssuer`, `... not valid for name ...`); the user's token is not sent.

## Budgets, audit and trust

Spend is charged to a workspace, so a commit moves an agent's cost from the engineer to the group on the day it happens.

- **Spend.** Each record in `spent.jsonl` (today: seat, model, tokens, cost) gains its workspace and steward. Budgets nest: the org gives each group a share, and each group gives each engineer a private allowance. The manager refuses a spawn past either, naming whose budget it is.
- **Machines.** Machines belong to the org and are lent to groups as pools. A private workspace runs on its engineer's own machine, or on a group pool within the engineer's allowance. When seats compete for a machine, group work comes before private, then the oldest waiting, so one engineer cannot starve the rest.
- **Audit.** The event log already records `by` on every command; commits, recalls, take-overs and forced releases add their own events. `wsp log --by <engineer>` and `--workspace <ws>` read it, and web-chrome shows it per group.
- **Trust between engineers.** Seats from different workspaces on one host run as one unix user today, so they could read each other's trees. A unix user per workspace on shared hosts is the real fence; until then, private work runs on its engineer's own machine.
- **Secrets.** Tokens are stored hashed, shown once and never logged. The instance token never leaves the server host. A seat only gets credentials its workspace holds.

## Scaling

The workspace is the unit that scales: state, streams, leases and machines are all cut along it, so adding a group adds a workspace, not load on everyone else's.

| Concern | Three engineers, one group | Many groups |
| --- | --- | --- |
| Store | One SQLite store on the server host, as today | The same `Store` contract on a server database; state-core already keeps fencing and idempotency in the contract, not in SQLite's single writer |
| Event stream | One log; each subscriber filtered to the workspaces it may read | A log per workspace, so a busy group never delays another's subscribers |
| Leases | `tick`, `barrier:<list>/<g>`, `sweep:<kind>` per instance | The same names scoped by workspace, so one group's barrier never waits on another's tick |
| Server | One process | Several processes, each serving a set of workspaces; the org's directory says which |
| Machines | One manager per machine, as today | Pools per group; a manager serves any workspace its pool is lent to |
| Clients | web-chrome subscribes to its engineer's workspaces | The same; the client never loads workspaces it cannot see |

The first build does the left column and keeps the right one possible: every record carries its workspace from day one, every lease and subscription is scoped by it, and nothing reads across workspaces except the org admin's views and commits.

## Phasing

&#91;embedded content: phasing · 6 phases, each with its gate\]

Phases 0 to 3 are the goal: three engineers, private workspaces, commits to one group. Phase 2 already stamps every record with its workspace, so phases 4 and 5 add budgets and scale without migrating data. Each gate is the test that closes its phase.

## Open questions

- [ ] Who are the first three engineers, and is it one group or two? One group is enough to prove commits; two proves isolation.
- [ ] Can an org admin read private workspaces in an emergency (a runaway agent), or only stop their seats?
- [ ] Does a committed agent keep its steward's git identity on commits, or take the group's?
- [ ] Is recall allowed after the group has built on a committed agent's work, or only before its row lands?
- [ ] A unix user per workspace on shared hosts: needed for the first three engineers, or only once private work runs on group pools?
- [ ] Single sign-on for the org from the start, or tokens first?
- [ ] Budgets in cost or in seat-hours? `spent.jsonl` records cost and tokens per seat, not time.
- [ ] `spent.jsonl` lines from concurrent seats interleave today; with three engineers writing, spend has to move into the store.
- [ ] Does D13 (identity for managers) settle in the same step, or stay one token per host?

## Breakdown

Planned 2026-11-19 (wsp-154; phases 1 to 3 by the lead the same day). The text above is the spec as it stood, unchanged. Six worklists, one per phase, hold the work. Nothing is started. Reviewed with the other two breakdowns by wsp-157 (2026-11-20): every list is planned at low priority but `mp5-scale`, which stays at init until state-core steps 3 to 5 have lists and have landed. Each planned list starts by itself once its `after` is complete, so a list's "before group 1" is owed by then. `mp1-identity`'s group 4 rests on `mp0-reach`'s compound-react-014, an `after` the run does not act on across lists (src/after.rs), so the barrier after its group 3 holds it. The phasing figure is not in the spec's text, so the gates of phases 0, 4 and 5 are read from their sections and are to confirm.

| phase | list | status | waits on | gate | gate row |
|---|---|---|---|---|---|
| 0 reach | `mp0-reach` | planned | `s2-state-core-2` | off the office LAN, a client reaches the server over the tailnet with no ssh tunnel | wsp-252 |
| 1 identity | `mp1-identity` | planned | `s2-state-core-2` | posing as another engineer is refused | wsp-167 |
| 2 workspaces | `mp2-workspaces` | planned | `mp1-identity` | B cannot see A's private rows | wsp-180 |
| 3 commit | `mp3-commit` | planned | `mp2-workspaces` | three engineers run compound together | wsp-192 |
| 4 budgets | `mp4-budgets` | planned | `mp3-commit` | a spawn past its budget is refused naming whose it is, and spend follows the commit | wsp-263 |
| 5 scale | `mp5-scale` | init | `mp4-budgets` | a busy group does not delay another group's subscribers or barriers | wsp-273 |

**The rows.**

- `mp0-reach`: compound-react-022 (web-chrome `--target` subscribes over the tailnet; rests on compound-react-027 in `cr-events-0`) and wsp-194 (doctor names where the API listens; the ACL recipe goes in "Reaching the server"); compound-react-014 (Needs you from the target's API); wsp-252 (gate).
- `mp1-identity`: wsp-158 (contract: the org), wsp-159 (the org machine, tokens), wsp-160 (the client's credential and `WSP_SERVER`), wsp-161 (the server derives `by`), wsp-162 (revocation drops connections), wsp-163 (`wsp org`, `group`, `engineer`), compound-react-028 (web-chrome dials with the engineer's token), wsp-164 (seat tokens), wsp-165 (`whoami`, attribution), wsp-166 (tokens by default), wsp-167 (gate).
- `mp2-workspaces`: wsp-168 (contract: workspace and version), wsp-169 and wsp-170 (the stamp in files and records, backfill), wsp-171 (the mirror), wsp-172 (where a new record lands), wsp-173 (scoped reads), wsp-174 (scoped subscribe), wsp-175 and wsp-176 (the door for people's commands, stale edits), wsp-177 (the CLI's reads), wsp-178 (presence), wsp-179 (Needs you in three bands), compound-react-029 (web-chrome), wsp-180 (gate).
- `mp3-commit`: wsp-181 (contract: stewards, commit, recall, give, pass), wsp-182 (the rights table), wsp-183 (stewards in the files), wsp-184 (the server checks rights), wsp-185 (commit a row or list), wsp-186 (commit and recall a running seat), wsp-187 (give, ask pass), wsp-188 and wsp-189 (the person's rules become the steward's or a lead's), wsp-190 (take-overs), wsp-191 (`wsp log`), compound-react-030 (web-chrome), wsp-192 (gate).
- `mp4-budgets`: wsp-253 (contract: spend, budgets, pools), wsp-254 (spend into the store), wsp-255 (the budget machine), wsp-256 (the spawn's gate), wsp-257 (`wsp budget`, `wsp burn --workspace`), wsp-258 (design: the fence on shared hosts), wsp-259 (pools and where a seat may run), compound-react-034 (web-chrome's budgets and audit), wsp-260 (group work first), wsp-261 (workspace secrets), wsp-262 (seats run as their workspace's unix user), wsp-263 (gate).
- `mp5-scale`: wsp-264 (design: re-plan against state-core steps 3 to 5, baseline), wsp-265 (contract: scoped leases, cursors, the directory), wsp-266 (leases per workspace), wsp-267 (a log per workspace), wsp-268 (server processes behind the directory), wsp-269 and wsp-270 (wsp-store-postgres), wsp-271 (one voice to a manager), wsp-272 (`WSP_STORE`, export and import), wsp-273 (gate).

**House rules, as in state-core's breakdowns.** Expand, then contract, each behind a flag that is the rollback (`WSP_IDENTITY`, `WSP_SPEND`, `WSP_LEASES`, `WSP_STORE`). Each phase opens with its contract change in a row of its own (R5). Rows in one group touch disjoint files. Each row's checks build the workspace and test every crate it touches. Each list ends at a gate row and then a step only the person can do. Principals, leases, fencing and the `Store` contract are state-core's; nothing here adds a parallel one. The org is the tenant of state-core D4 and 8.6; a workspace is a scope inside it, not a tenant.

**What the plan adds to the spec, because the code needs it.** Rows, lists, seats and asks are file-owned until state-core steps 3 and 4, so a remote engineer's writes go through a server-side door the root implements (wsp-175, wsp-176). No secret crosses the manager's wire, so the manager mints the seat token and reports its hash (wsp-164). The spec's `stale` is the contract's `conflict`. "Workspace" already names a host pane's workspace, so the new id is `WorkspaceId`, and `ws` in JSON that carries both.

### Assumptions to confirm

Each is stated in the row it affects. The recommendation is the planning row's; the person decides.

1. **Order against Ed's rule.** state-core section 7 holds sharing until step 6. These lists pull step 6's auth forward on purpose; mp1 needs only s2-state-core's wsp-117, wsp-118 and wsp-121, and is set `after s2-state-core-2`. Recommend: keep that, and tell Ed.
2. **Tokens first, no single sign-on.** Shown once, stored as hex SHA-256 (wsp-159, wsp-163). Recommend: accept; sign-on later replaces only how a token is issued.
3. **The instance token speaks for machines only**, and D13 stays one token per manager host (wsp-161, wsp-166). Recommend: accept.
4. **The manager mints seat tokens** (wsp-164). Recommend: accept.
5. **Out of scope is `forbidden`**, which shows that an id exists; listings omit it (wsp-173, wsp-177). Recommend: accept, as the spec says.
6. **An admin reads every group workspace and no private content**; a runaway private seat is stopped from the server's host (wsp-173, wsp-182). Recommend: accept; "stop, never read".
7. **Existing records go to one group workspace, `group:fleet`** (wsp-168, wsp-169, wsp-171). Recommend: accept, with the first group's real name chosen before `wsp org init`.
8. **The server's host is trusted; no unix user per workspace in phases 1 to 3** (wsp-177). Recommend: accept; wsp-258 decides the fence in phase 4.
9. **`stale` is `conflict`** (wsp-168, wsp-176). Recommend: accept.
10. **Project slugs stay org-unique; one repo is several projects**, matched by repo root at commit, the row's id kept (wsp-169, wsp-185). Recommend: accept.
11. **Recall only before the row lands and while nothing rests on it** (wsp-186). Recommend: accept.
12. **Give only inside a group workspace** (wsp-187). Recommend: accept.
13. **A seat is told by its steward, a lead of its group, or its governor**, not by every engineer of the group (wsp-184). Recommend: accept.
14. **A committed agent keeps its steward's git identity.** Recommend: accept; the commit event records the group.
15. **"Steward away" for the My groups band is 2 h**; away itself is 30 min (wsp-179, wsp-178). Recommend: accept, both settable.
16. **Leases keep their names until phase 5** (wsp-168; done by wsp-265, wsp-266). The spec wants them scoped from day one. Recommend: accept; leases are transient, and the server's leases are state-core step 5's.
17. **Phase 3 has only the budget hook**, `Seat.budget` (wsp-181). Recommend: accept; phase 4 is filed.
18. **The gates use one group, a watcher, and an engineer in no shared group.** Recommend: accept for the tests; the person names the first three engineers for the live run.
19. **An admin holds a lead's rights in every group** (wsp-182), more than the spec's table gives. Recommend: keep, each such act a recorded take-over.
20. **Budgets are in cost, per calendar month** (wsp-253). Recommend: accept; `spent.jsonl` records cost, not time.
21. **An engineer's private allowance comes out of one home group's share** (wsp-255). Recommend: accept.
22. **wsp refuses a spawn past budget, not the host's manager** (wsp-256). Recommend: accept; the manager holds no org state.
23. **No cap per engineer on a pool**; group-first order and the allowance are the fairness (wsp-260). Recommend: accept.
24. **Seats and asks carry no version**; their state refuses a second end or answer (wsp-168). Recommend: accept.

**The spec's open questions**, in its order: first engineers and groups (18); an admin and private workspaces (6); git identity (14); recall (11); a unix user per workspace (8, wsp-258); single sign-on (2); cost or seat-hours (20); spend into the store (yes: wsp-254); D13 (3).

### Not filed

- **Whether a private workspace's row may land on a repo's shared trunk.** The spec says "landing on the trunk stays the group's barrier rules" and no row says what a private row's landing is. Recommend: a private project lands on a branch of its own, and the trunk is reached by a commit to the group. It needs the person's answer, then a row in `mp2-workspaces` or `mp3-commit`.
- **Single sign-on**, which the spec leaves for later.
- **state-core steps 3 to 5**, which `mp5-scale` rests on and which have no lists yet.
- **The live moves**: Tailscale and its ACL, `wsp org init`, the backfill, the real budgets, a store's move to Postgres. Each is a list's last stop, the person's.

**What `wsp doctor` says of the hub** (wsp-194). One line for the local API, naming the addresses `api.json` lists. Then, for a wsp with no login, one line: `not logged in to a hub (standalone)`, and nothing else about hubs. For a spoke: the hub URL and user (from `hubs.json`); the certificate's expiry (read from a handshake with the URL's host, so a `tcp://` hub says it has none; a week or less is a warning); the last good sync and the last failure with its reason (`$WSP_STATE/hub/sync.json`, which the worker keeps); the journal's length; the row leases the claims here hold and the soonest expiry; the drive leases held. A lease within a day of its expiry whose seat is not alive is a warning, since nothing renews it. No host or address is a literal: everything is read from `api.json`, `hubs.json` and the worker's files.
