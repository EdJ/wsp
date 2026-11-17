# The agent manager: every host's local operations behind one process

Plan `wsp-652`, 2026-11-16. **This is a draft for Ed to discuss, not a design he has approved.** It changes no code
and files no rows. Where it recommends something, the options are beside it. The open points are the numbered
questions in section 7.

**Diagram: [agent-manager.svg](agent-manager.svg)**. The top panel shows the target. The bottom panel shows what
crosses the host boundary today and is removed.

Ed, 2026-11-15, prompting the server conversion (wsp-483, D12): *"let's start with planning for the wsp server
conversion - I think we should start by segregating local operations into an "agent manager" such that each agent
host becomes fully delineated from wsp"*.

**What this plan takes as settled** (the decisions on wsp-483):
- D5: tree work stays where the tree is, and only its record moves to the server.
- D5b: machine facts come from executors. This is a working choice, open to correction.
- D8: the server does not run on the UI's machine or on an agent's machine.
- D14: Ed leans towards "one central process that spawns durable supervisors".
- D15: there is no herdr executor.
- D19: network transport comes from the start.
- D20: wsp-120, herdr's removal, is a line in the first pass.

Ed's newer remark (2026-11-15) reopens D5's *where*, and it is raised as Q6: *"checkouts etc will likely need to
live with the agents in the future, but let's discuss"*.

**Short answer.**
1. **The seam moves first.** state-core put executors at step 3, behind the server. This plan puts an **agent
   manager** on every agent host *before* the server (step M, section 6). The manager is one long-lived process per
   host, under launchd. It owns every operation that is local to its host: starting an agent, stopping one, the
   agent's process tree, delivering an order and hearing it acknowledged, the hooks, transcripts and spend, the
   host's load, memory, disk and power, and the processes of tree work. wsp (the CLI and the daemon now, the server
   later) reaches it only through one protocol (section 3). The Mac is a host like the i5. Nothing in wsp assumes that
   it shares a machine with an agent.
2. **It is mostly code that exists.** The per-seat durable supervisor is compound-sup, and it stays as it is.
   compound-remote's `serve` is already a per-host controller: one socket, `spawn`/`list`/`close`/`lease`/`version`,
   and no state of its own beyond the sockets (compound crates/compound-remote/src/server.rs:24, sessions.rs:1). The
   OS half of wsp's `Place` backends, `far.rs`, the tree copy and the probes move from wsp into the manager. Its
   decisions stay in wsp.
3. **The protocol is state-core 5.1, with three changes.** The manager exists before any server, so on day one the
   wsp side dials it. It listens on TCP from the start, including on the Mac (D19), so the local host uses the same
   path as a far one. Its events carry ids, and a reconnecting client asks for them `from` the last one it saw. Day
   one's fencing is idempotency keys and seat epochs. Leases between hosts arrive with the server.
4. **The largest change on the wsp side is what it stops guessing.** Who owns a process, whether a seat is alive,
   whether an order was taken and whether a seat has ended all become events the manager sends. On the last day's
   figures that removes about 37 `ps`, 60 screen renders, 25 `df` and 1 `lsof` per daemon tick, and the 41,459 `ps`
   and 195,490 renders that compound's host polling cost in 24 hours.
5. **Tree work is the open question.** The recommendation is to put tree work's *processes* under the manager now,
   so the manager owns them and the `lsof`/`ps` sweeps go. The *trees* stay where D5 put them for the present, and
   Ed decides whether they move to the agents' hosts (Q6). Section 1.3 gives the cost of each choice.

---

## 1. The boundary

### 1.1 How it was taken

The inventory below was taken from the code at `ee8c1de` (the trunk this row started at). The counts are from
`grep` over `src/` and `crates/`, leaving out lines after a file's trailing `#[cfg(test)] mod`. Costs are from
`wsp calls --since 24h` at 2026-11-16T06:48Z: 1,633 daemon ticks, 43,898 calls and 146,483 CPU-s.

Per tick, averaged over those 1,633 ticks (the `"k":"tick"` lines' `children`):

| child | per tick |
|---|---|
| git | 162.5 |
| compound-render | 59.5 |
| ps | 37.5 |
| df | 24.6 |
| sysctl | 3.5 |
| pmset | 1.9 |
| memory_pressure | 1.3 |
| vm_stat | 1.3 |
| kill | 1.3 |
| lsof | 0.9 |

The tick's p50 is 19.5 s and its p95 116 s. On 2026-11-08 they were 2.2 s and 54 s.

Several of state-core 1.4's sites have moved since `47126dc`:
- check_tree's second `lsof` is now :579, not :507.
- `Shells::read` is waiting.rs:745 (its `ps` is at :762), not :355.
- `read_screen` is place_compound.rs:808, not :746.
- `typed` is :1972, not :1824.
- `turn_began_since` is :2027, not :1875.

The lines below are current.

Each line has one of four verdicts:
- **M**: it moves into the manager.
- **W**: it stays in wsp.
- **D**: it is deleted, because an event replaces the inference.
- **C**: it stays compound's.

### 1.2 The inventory

**Starting an agent**

| operation | site | verdict | why |
|---|---|---|---|
| The `Place` port: `open`, `start`, `tell`, `typed`, `stop`, `state`, `turn_began_since`, `pids`, `census` | crates/wire/src/place.rs:1032–1271 | **W (shape), M (bodies)** | The port is the wsp side of the protocol. Its implementors are the OS half and they move. |
| compound seat: `sup_command` builds `compound-sup spawn --label --agent --cwd`, `spawn_sup` runs it, `open`, `start` | crates/wire/src/place_compound.rs:1364, :1430, :1821, :1866 | **M** | Forking a supervisor is local to the host. |
| headless seat: fork and pipe, observed through hooks | crates/wire/src/place_super.rs:1330, :1382 | **M** | The same. |
| far seat: a shell line run over ssh that starts `compound-sup` and greps its handshake | crates/wire/src/far.rs:488 (`launch`), :125 (`ssh_run`) | **M (the far manager does it locally); D (the ssh line)** | The i5's manager forks compound-sup itself. No handshake needs to cross ssh. |
| what the agent is run with: argv, environment, the opencode config, the brief | src/agent_commands.rs:1198 (`config`), `Kind::env`; crates/wire/src/place.rs:344 (`CHILD_MARKER`) | **W** | This is policy. wsp composes an *agent spec* and sends it in `seat.spawn`. The manager runs what it is given. |
| handing over the first order, then waiting until a turn starts on it | src/cmd_spawn.rs:1732 (`hand_over`), :1824 (`took_it`) | **W (the decision); D (the polling)** | wsp waits for the manager's `order.acked` event instead of polling `state` and `typed`. |

**Stopping and signalling**

| operation | site | verdict | why |
|---|---|---|---|
| A seat's process tree from `ps`, then a kill by pgid | crates/wire/src/place_super.rs:844, :886; crates/wire/src/place_compound.rs:1298 | **M** | The manager started the tree, so it ends it and reports the pids. |
| leaked agents: agents found by `ps` and killed | src/leaked_agents.rs:126 | **D** | Every agent on a host is one of the manager's seats. An agent that is nobody's cannot exist, except one started by hand, and that is not wsp's to kill. |
| `wsp role reap`: `ps`, `lsof` and kill | src/cmd_role.rs:360, :629, :671 | **D**, or a manager verb if Ed wants a person's reap | The same reason. |
| a shell waiting on itself, found by `ps` and killed with TERM | src/waiting.rs:872, :875 | **M** | The shell is in the seat's tree. The manager reports `seat.stuck` and wsp decides whether to end it. |
| strays in a finished tree: `lsof -d cwd`, then kill (wsp-478) | crates/trees/src/check_tree.rs:221, :259, :327, :579, :592 | **D** once tree jobs run under the manager (1.3) | Ownership becomes a record, not a cwd. |
| a check's own child group, killed on timeout | src/check.rs:1249, :1261, :1272; crates/trees/src/at_stop.rs:98 | **with tree work** (1.3) | The process holds a handle on the child it spawned. This is not inference. |
| a sandbox's agents | src/cmd_sandbox.rs:507 | **M** | A sandbox is a set of seats on a host. |

**Asking the OS about processes**

| probe | site | verdict | replaced by |
|---|---|---|---|
| `ps -A -o pid,ppid,command` and `ps -A -o pid,ppid` | crates/base/src/base/procs.rs:20, :39 | **D** (wsp side) | events with the pid root that the manager recorded |
| a seat's running shells, per `wip` | src/waiting.rs:745, :762 | **M** | `seats` replies carry `shells` |
| memory per seat | src/memory.rs:122, :145 | **M** | per-seat RSS in the heartbeat |
| is a recorded pid alive | crates/base/src/util.rs:1536 | **D** for seats; **with tree work** for build slots | `seat.stopped` |
| build-slot holders alive; a check's pid identity; check liveness | crates/store/src/sharing.rs:390; crates/store/src/checks.rs:144; src/check.rs:1119, :1173; crates/trees/src/gc.rs:54; crates/trees/src/throttle.rs:839 | **with tree work** (1.3) | `job.ended` |
| ended panes: `lsof` for a renderer whose agent has exited | src/ended_panes.rs:81 | **D** | `seat.stopped`, which says whether the renderer is left |

**Host facts**

| fact | site | verdict | why |
|---|---|---|---|
| thermal state (`pmset -g therm`), load (`sysctl vm.loadavg`), memory pressure, swap, `vm_stat` | crates/trees/src/throttle.rs:307, :322 (`load1` :337), :652, :694, :702 | **M (reading); W or tree host (deciding)** | They are facts about a host (state-core correction 2). They go in the heartbeat every 10 s. The throttle's admission decision is made where the build runs (Q11). |
| disk free and use (`df -Pk`, `du -sk`) | src/disk.rs:120, :214 | **M** | Disk belongs to a host. Today `df` runs 10,555 times a day, 24.6 per tick. |

**Screens and panes**

| operation | site | verdict | why |
|---|---|---|---|
| render a seat's screen and read the composer (is an order typed but unsent; the trust prompt) | crates/wire/src/place_compound.rs:808, :423, :1972; callers src/repair.rs:650, src/cmd_spawn.rs:1824 | **D** with compound-sup's `deliver` and ack (wsp-477, compound supervisor/src/proto.rs:252); **M** for a person's `peek` | The 195,490 renders that compound's host polling made in 24 hours go. A person's peek becomes a `seat.read` request to the manager. |
| herdr `pane.read` | src/cmd_agent.rs:5745 | **D** (wsp-120) | There is no herdr executor (D15). |

**Hooks, turns, spend and transcripts**

| operation | site | verdict | why |
|---|---|---|---|
| `wsp hook`: the agent's hooks write `compound-seats/<seat>/hooks.jsonl`, `said.json` and `burn.json` under wsp's state | src/cmd_agent.rs:6103; crates/wire/src/place_compound.rs:255; crates/wire/src/place_super.rs:367, :904 | **M** | The hook fires on the agent's host and reports to that host's manager. The manager folds it into `turn`, `seat.said` and `seat.spent` events. wsp's state directory no longer holds per-seat files. |
| did a turn start since T | crates/wire/src/place_compound.rs:2027 | **D** | `order.acked` and `turn` events |
| reading a transcript (path, digest, spend, last word) | src/agent_commands.rs:2723; src/did.rs:463; callers in timeline, cmd_verifications, `attempts` | **M** | The transcript is on the agent's host (on the i5 it already is). The manager reads it and sends digests. wsp keeps a reference (host, path) for a person who wants the whole file. |
| opencode's session export and `opencode db` reads | src/agent_commands.rs:1687, :1774 | **M** | The same reason, for opencode. |

**Delivery**

| operation | site | verdict | why |
|---|---|---|---|
| `tell` through the backend | crates/wire/src/place.rs:1118; place_compound.rs:1904 (compound-sup `deliver`); place_herdr.rs:781 | **M** | The manager calls compound-sup's `deliver` on a local socket and answers `delivered` or `refused`, with the reason. |
| the order queue, the held tells for an unreachable seat, flushing them | src/wake.rs:540, :566, :607 | **W** | Policy: who gets told what, and when. |
| retyping unsent text, a first order owed, a shell waiting on itself | src/repair.rs:650, :937, :788 | **D** (unsent, first order); **M** (stuck, reported as `seat.stuck`) | This is the I3 inference. |

**Far machines, ssh and launchd**

| operation | site | verdict | why |
|---|---|---|---|
| the tunnel supervisor: one ControlMaster per machine, plus per-seat `-O forward` | crates/wire/src/tunnel.rs:460, :744; daemon phase src/daemon.rs:768 | **D** | wsp holds one TCP connection to each host's manager, and nothing per seat. |
| the beat: `lease <seat> ok` through each far seat's forwarded controller socket, paced by the tick (wsp-639) | crates/wire/src/far.rs:1124, :978, :971, :779; tunnel.rs:185 | **D (the beat); M (the lease file)** | The connection's own heartbeat replaces it (section 3.4). The manager writes the gate's lease file locally. |
| the far manifest and the tree copy (wsp-638: 4 min 25 s for 8,788 files) | crates/trees/src/treecopy.rs:612, :709, :969, :1093 | **with tree work** (1.3); the far half **M** | The manager hashes in-process on its host, with no fork per file. |
| `machine check`: wire version, a throwaway session, a headless `claude -p` | crates/wire/src/far.rs:80 | **M** (`hello` reports the wire, `check` runs the session); **W** keeps the verdict | |
| the daemon's own launchd agent | crates/wire/src/launchd.rs:297, :393, :418, :465, :558 | **W** | It is wsp's process. The manager has its own launchd agent (section 2.4). |

**herdr.** crates/wire/src/herdr.rs (951 lines) and crates/wire/src/place_herdr.rs (1,598) remain. Outside them there
are 264 non-test references in about 30 files: cmd_agent 64, panel/verbs 22, detail/run 19, cmd_govern 16,
panel/install 15, cmd_spawn 14, and others. **All of it is D, as wsp-120, at step M0** (section 6). The manager
never learns herdr.

### 1.3 Tree work: where it falls

Tree work is `checkout` (crates/trees/src/cmd_checkout.rs:1799), `check` (src/check.rs:1669), `verify`
(crates/trees/src/cmd_verify.rs:1244), `land` (cmd_checkout.rs:2666), barrier runs (src/barrier_run.rs), builds under
the throttle (crates/trees/src/throttle.rs:1200 `run_in`, :1742), warm targets (crates/store/src/sharing.rs:145) and
the tree copy (treecopy.rs). It is two-thirds of wsp's CPU: `throttle run` 56.1% and `check` 15.2% of 146,483 CPU-s in
24 hours.

There are two separate questions here.

**(a) Who owns tree work's processes.** The recommendation, for any placement of the trees: **the manager on the
tree's host runs tree work as jobs.** A job is `job.run {key, argv, cwd, class}` and it ends with
`job.ended {exit, log}`. The throttle's slots become the manager's admission per class, either `build` or `check`.
The manager's job table is then the record of who started what in a tree. The cwd guess behind wsp-478, the `ps`
walk in check_tree and the slot-liveness probes in sharing.rs and checks.rs all go. Today the tree's host is the Mac,
so this changes no placement.

**(b) Where the trees live.** Ed: *"checkouts etc will likely need to live with the agents in the future, but let's
discuss."* There are three options.

| | T1. Trees stay on the store's host (D5 as written) | T2. Trees live with the agents, under each host's manager | T3. Working trees with the agents, the trunk and its land on one trunk host |
|---|---|---|---|
| what | Today. The Mac holds every row's worktree. A far host gets a copy and sends work back (remote-agents v1 §6). | Each agent host checks out its rows' trees from the trunk repository. `check` and `verify` run there. `land` rebases there and pushes to the trunk, with a fenced push. The barrier runs on a host the list names. | Generalised remote-agents v1. Every host works in its own copy. The trunk host (the Mac today) receives copy-backs and does every land and every barrier. |
| copies | One per far row, both ways (wsp-638's cost). | None. git fetch and push only. | One per row, both ways, local rows included (a no-op on the trunk host). |
| git as transport | No (Ed's 2026-11-05 decision for v1). | **Yes.** That reverses the 2026-11-05 decision "copied, not pushed git to git", which was made for far seats. | No. |
| warm targets | One set, on the Mac. | One set per host per project. The i5 starts cold, and so does a third host. sccache (already used for SQLite, D11) can share compiled objects between hosts. | Per host for the agents' own iteration. The verdict is built on the trunk host, so it has two sets. |
| disk | The Mac holds everything (`barrier/` 37 GB, `checks/` 34 GB in state-core 1.3). | Spread across hosts, with every host's manager running its own sweep. | The Mac still holds every tree that is verified. |
| the trunk | The Mac's repository. | A remote every host can reach: a bare repository on a trunk host, or the forge. A person's checkout becomes a clone that pulls. | The Mac's repository. |
| verify and land | On the Mac. A heavy row's verifier already runs on the i5 (remote-agents 11.3). | On the tree's host. A land is a push with `--force-with-lease` against the trunk head the verifier read. That push is the fence, so two hosts cannot both land on one head. | On the trunk host, after copy-back. |
| load moved off the Mac | Heavy rows only (remote-agents §11). | All work placed on another host. | The agents' iteration only. Verdicts, lands and barriers stay on the Mac. |
| what the manager needs | Jobs, and the far half of the copy. | Jobs, checkout, rebase, push, the sweep of its own trees, a trunk fetch on each spawn. | Jobs, and both halves of the copy for every row. |

**The recommendation.** Build (a) now, at step M3, whatever is chosen for (b). Plan the protocol so that (b) changes
placement, not architecture:
- jobs name a tree by `(row, host)`, never by a path on wsp's machine;
- `checkout` and `land` are manager verbs from the start, even while every manager that runs them is the Mac's.

T1 then stays the running state until Ed decides between T1 and T2 (Q6). T3 is listed for completeness. It keeps
every cost of the copy and moves the least load. It earns its place only if the trunk must stay on one machine for a
reason this plan does not know.

**What T2 costs, in work.**
1. A trunk remote per project that each host can reach. Hosts are on the tailnet, so this can be a bare repository
   on the Mac served over ssh. That uses the ssh door, not a new one.
2. A fetch at each spawn and a push at each land, by the manager, with the push fenced as above.
3. Warm targets per host (an i5 cold build is measured in remote-agents §11.5's early proof). sccache narrows the
   gap.
4. The barrier's host named per list. The default is the trunk's host, which is remote-agents' rule that "the
   barrier stays here".
5. `wsp checkout` by a person: either on the person's own host, which then has a manager, or refused with a pointer
   to the host.
6. The remote-agents copy machinery (treecopy.rs, 1.5k lines with its tests) is retired once no host uses T1 for far
   rows.

---

## 2. The manager's shape

### 2.1 One manager per host, supervising durable per-seat supervisors (recommended)

```
launchd (per host, the logged-in user's session)
 └─ the agent manager             one per host: protocol, seat table, journal, jobs, facts, hooks endpoint
     ├─ compound-sup  <cpd-N>     one per seat, setsid, durable: pty, child, deliver/ack (unchanged)
     │   └─ the agent (claude / opencode) and its tool processes
     ├─ compound-sup  <cpd-M>
     └─ job  <check|build|verify> one per tree job, its own process group
```

This is D14's lean: "one central process that spawns durable supervisors". The manager is the one central process
*per host*. compound-sup is the durable supervisor, one per seat, and it outlives the manager (2.5).

The alternatives:

| | shape | for | against |
|---|---|---|---|
| **A** (recommended) | One manager per host, and one compound-sup per seat beneath it | One connection per host. Facts are read once per host. A manager restart loses no agent. A seat's crash stays the seat's. It matches compound-remote `serve` and compound-sup as they are. | One more long-lived process per host. |
| B | No manager. wsp speaks to each compound-sup directly over the network: one socket per seat, which is today's far shape. | Nothing new to run. | One forward per seat, as today. Facts, hooks, transcripts and jobs have no home on the host, so wsp still reaches in. This is the status quo. |
| C | One supervisor for the whole host: compound-sup grown to many sessions | One process | One crash ends every seat. It reverses compound's supervisor-per-session design (remote-agents §2). |
| D | The wsp daemon runs on every host and is its manager | It reuses the most code. | wsp's whole binary and store logic would live on each agent host. It blurs the boundary this plan draws, and a second daemon raises the `daemon.json` singleton question again. |

### 2.2 What it is built from

- **compound-sup, unchanged.** Spawn, vitals, close, `deliver <id>` with its at-most-once answer, and `submitted`
  from the agent's `UserPromptSubmit` hook (compound crates/compound-sup/src/main.rs:43,
  crates/supervisor/src/proto.rs:238–262). The manager is its only caller on a host.
- **compound-remote `serve`, as the seed of the process.** It is already one controller per host: a socket in
  `<exec_dir>`, a thread per connection, and `spawn`/`list`/`close`/`lease`/`version`. It keeps no state beyond the
  sockets ("the sockets under the executor directory are the list", sessions.rs:1). It also holds the Claude token
  for the child (proto.rs `TOKEN_ENV`). The manager needs that loop, that listing and that token handling. Q2 asks
  whether the manager *is* compound-remote grown, or a separate process that uses it.
- **compound-remote `gate`, unchanged.** It reads `<exec_dir>/lease/<seat>`. The manager writes that file, where a
  beat through a forward writes it today (section 5).
- **wsp's far and OS code.** It moves out of wsp and into the manager:
  - the OS halves of place_compound.rs and place_super.rs: spawning, the pid tree, stopping, the hook files and spend
    tallies;
  - far.rs's `machine check` session;
  - treecopy.rs's far half: the manifest, computed in-process;
  - waiting.rs's shell reader, memory.rs, disk.rs's `df`/`du`, throttle.rs's fact readers;
  - the transcript readers in agent_commands.rs and did.rs.

  Their *decisions* stay in wsp: which seat, what agent spec, when to end it, what counts as stuck, whether to admit
  a build.

### 2.3 Which repository and crate

| option | where | for | against |
|---|---|---|---|
| **R1** (recommended) | **wsp's repository.** A crate `crates/manager` (binary `wsp-manager`; the name is a placeholder) and a protocol crate `crates/host-api`, with types only, as R2 of components.md. The binary depends on host-api, wsp-base and the moved OS code, and on *nothing* of wsp-store. | Most of the code that moves is wsp's. The protocol and both of its sides change in one repository, under R5 (a contract changes only by a row of its own). It needs no compound release to change a transcript reader. | It runs compound-sup as an external binary, as wsp does today, and must match its wire version. |
| R2 | **compound's repository**, growing compound-remote into the manager | It sits next to compound-sup and shares `supervisor::proto`. | wsp-specific readers (opencode's database, the stuck-shell matcher, the throttle's facts) land in compound, and every wsp change to them becomes a compound row. |
| R3 | A third repository, holding the manager and host-api | It is neutral. | A third release train and a third place to look. |

Under R1, state-core section 6's "executor: in compound (`compound-sup`)" becomes "the manager, in wsp, over
compound-sup". `wsp-api`'s executor messages become `host-api` (section 3).

### 2.4 Installed and kept running

- **launchd today.** There is one LaunchAgent per host, `<label>.manager`, in the user's GUI (Aqua) session. That is
  where compound-remote already has to run for the keychain and token (remote-agents §8). `KeepAlive` is true and
  `ThrottleInterval` is 5 s. It writes its own log in `<exec_dir>/log/`. The plist is written the way
  crates/wire/src/launchd.rs writes the daemon's.
- **Install and upgrade over the ssh door.** `wsp machine install <host>` copies the binaries at one commit,
  writes the plist and the manager's token, and loads it. That is today's `scripts/remote-ship.sh` path. On the Mac
  `wsp install` does the same locally.
- **Linux or a container later**: a systemd unit or the container's init. Only the service file differs.
- **One per host.** It binds its port and refuses to start if another manager answers there, as compound-remote
  `serve` refuses (server.rs:28).

### 2.5 What survives a manager restart

| thing | survives? | how |
|---|---|---|
| the agents and their conversations | **yes** | compound-sup is `setsid` and daemonized. It does not depend on the manager. |
| the seat table | **yes** | The manager rebuilds it from the sessions' sockets in `<exec_dir>` (as compound-remote's `list` does) and joins them to its journal by label. |
| an order in flight | **yes** | compound-sup owns the delivery once it is asked, and answers the same id with the same answer (proto.rs:246). The manager asks again with the same id on restart. |
| events not yet heard by wsp | **yes** | The manager's **journal** (`<exec_dir>/journal.jsonl`) is appended before an event is sent. Ids are a per-manager sequence. A client reconnects and asks `from` its last id. |
| a tree job | **mostly** | A job runs in its own process group under a small wrapper that writes its exit to `<exec_dir>/jobs/<key>`. A job that finished while the manager was down is reported on restart. A job still running is re-adopted by pid and start time. |
| host facts | **not needed** | They are read again on the next heartbeat. |
| the gate's lease files | **yes, but they age** | A restart that outlasts the lease holds the far seats' tool calls. That is correct: nobody can hear them. |

A wsp restart (the daemon or the server) touches none of this. wsp reconnects and asks `from` its last id.

---

## 3. The protocol

### 3.1 Base and changes

The base is state-core 5.1's message table. The changes follow from the manager existing before the server.

1. **The wsp side dials, on day one.** The CLI and the daemon open a connection to each host's manager. Later the
   server does the same. 5.1 had the executor dialling the server. That needs a server, and before one exists the
   only long-lived wsp process is the daemon on the person's Mac. Q4 asks whether managers should dial out once the
   server exists: a cloud host can only make outbound connections. The messages are the same in both directions. Only
   who opens the connection changes.
2. **The manager's events carry ids**, and a client asks `subscribe {from}`. This is the shape of state-core 4.3, at
   host scale. The daemon holds one subscription per manager and folds the events into today's files. The server will
   fold them into the store.
3. **No leases between hosts on day one.** Fencing is by idempotency keys and seat epochs (3.3). Executor leases
   (5.1's `lease`) arrive with the server, where there are several instances to fence.

### 3.2 What wsp sends and what it hears

| message | direction | carries | replaces |
|---|---|---|---|
| `hello` | both | wsp: role (`cli`, `daemon`, later `server`), version, the client's last event id. Manager: host id, manager version, the compound-sup and compound-render wire range, journal head, seats it runs, capacity. | `far::probe_command`, the wire check over ssh, `wire_health` |
| `seat.spawn` | W → M | key `(row, attempt)`, the wsp-minted seat id, the agent spec (argv, env *names*, kind, model), the tree `(row, host)` or cwd, `gate: required \| none`, the first order (optional) | `Place::open` + `start`, compound-sup over ssh, per-seat forwards |
| `seat.started` / `seat.refused` | M → W | seat, epoch, pid root, sockets (for a renderer to attach) / a named reason | 150 ms `state` polling (crates/wire/src/place.rs:1184, :1303) |
| `seat.stop` | W → M | seat, epoch, `grace` | every `kill` of a seat's pids found by `ps` |
| `seat.stopped` | M → W | seat, epoch, exit, the pids it ended, whether a renderer is left | census-by-absence (I4), `ended_panes`, `finished-wip.json` |
| `order.deliver` / `order.cancel` | W → M | order id, seat, text, `now` | `Place::tell`, screen reads |
| `order.delivered` / `order.refused` / `order.acked` | M → W | order id; for a refusal, compound-sup's `FailWhy` (busy, no composer, refused, session gone, no renderer, …); for an ack, the turn's start | `typed-unsent.json`, `repair:unsent`, `first-orders.json`, `turn_began_since` |
| `turn` | M → W | seat, `began` or `ended`, order id if any | `seat-since.json` sampling |
| `seat.said` / `seat.spent` / `seat.did` | M → W | last word; tokens and cost; a digest of the turn, with a transcript ref `(host, path)` | `said.json`, `burn.json`, `spent.jsonl` writes; transcript reads in wsp |
| `seat.stuck` | M → W | seat, the command, how long it has waited on itself | `stuck_tick`'s `ps` |
| `seats` | W → M (request) | — ; the reply has per seat: state, epoch, rss, shells, last turn, last word | `wip`'s `ps`, `lsof` and renders |
| `seat.read` | W → M (request) | seat; the reply is the screen text | `peek` (a person asked for it, never a tick) |
| `job.run` / `job.stop` | W → M | key `(row, attempt, job)`, `job` unique per run; class (`build` or `check`), argv (with the env prefix), where: a tree `(row, host)` or a dir `(host, path)` under the host's tree root; timeout; `hung_after_secs` (optional); `detach` | `throttle run`'s slots, check's own kill on timeout and on hung (wsp-609), check_tree's sweep |
| `job.started` / `job.ended` | M → W | key, pgid / exit, timed out, hung, log ref, CPU-s | slot liveness probes, `kill -0` |
| `job.output` | M → W, to the asking connection only | key, `from` (byte offset in the log), text; not journalled, and the log ref stays the whole log | `throttle run`'s and `verify`'s own echo of cargo |
| `jobs` | W → M (request) | — ; the reply `jobs.list` has per job: key, class, pgid, start, `running` or `ended` | the slot probes |
| `tree.checkout` / `tree.land` / `tree.copy` | W → M | only under T2 or T3 (1.3); listed so they are reserved | — |
| `beat` | both | every 10 s on each side's own timer. The manager adds host facts (load, memory pressure, swap, disk free per volume, thermal state) and per-seat RSS, and the throttle's facts, each optional both ways (wsp-044): 5- and 15-minute load, speed-limit percent, HID idle seconds, free memory, pageouts. | `df` ×24.6, `sysctl`, `pmset`, `vm_stat`, `memory_pressure` per tick; the far beat |
| `events` / `subscribe` | W → M | `from` id | — |

**What crosses in `seat.spawn`'s environment** (wsp-026 d1). The agent spec carries env *names*, and the manager
fills them from its own environment. The values wsp mints for each seat, such as its row and its store, are not in
the manager's environment and are not secret. They cross as an `env -u K -- K=V` prefix on the argv. A secret, such
as a token or a key, never goes in that prefix: it is a name the manager fills, and a secret found in the prefix is
a defect.

**Jobs** (wsp-044, M3 group 1). What the job rows above leave to the types, decided:

- *Where a job runs.* A job runs in a row's tree, named `"tree": {row, host}` as before, or in a place that is no
  row's: a check tree (`checks/…`), a barrier's tree (`barrier/…/tree`), a warm target. That place is
  `"dir": {host, path}`, where `path` is relative to the host's tree root, never a path on wsp's machine. The
  manager refuses `no_tree` for a path that falls outside that root: absolute, climbing above it with `..`, or
  absent. `HostDir::escapes` is the lexical half of that check. The two sit where `tree` sat, so a `job.run` from
  before this row still decodes.
- *The key.* `JobKey.job` names one run, not a kind of job. A retry of the same run sends the same key, and a new run
  sends a new one: the caller appends the run's own mark (`check@1763400000000`). wsp-040's reopened row reused
  `(row, 0)` and was refused, which is why the name alone is not enough. A job with no row, such as a barrier's, puts
  `<list>#<group>` in `row` (`m3-agent-manager#1`) and the barrier's round in `attempt`.
- *The bounds.* `timeout_secs` ends a job after that long whatever it does (`timed_out`). `hung_after_secs`, when
  given, ends it after that long with no output and no CPU (check.rs's wsp-609 bound; `hung`).
- *The environment.* The rule of `seat.spawn`, above: values wsp mints cross as the argv's `env -u K -- K=V` prefix,
  and a secret never does.
- *Ownership.* A job that is not `detach`ed is stopped when the connection that asked for it closes, so a
  `throttle run` killed with its agent leaves no build behind. `job.output` goes to that connection and no other.

The manager served none of these until M3's group 3 (wsp-050), which is written up under 6.2.

The manager does **not** decide anything that is wsp's to decide. It does not choose a seat, pick a row, end a seat
of its own accord (except at the gate's existing rule, section 5), hold a tell, or judge a check's output. It does
refuse a malformed or duplicate command, and every refusal it makes carries a reason.

### 3.3 Identity and fencing, kept to day one

D13 leaves identity for later, so day one carries only what stops the fleet from hurting itself.
- **A manager token.** The manager makes it at install, keeps it at mode 0600 in `<exec_dir>`, and wsp keeps a copy
  in the machine record's secret file. `hello` must carry it, and a connection without it is closed. It is one token
  per host, not one per seat. Per-seat and per-person principals are state-core step 6 (D13).
- **Idempotency keys.** `seat.spawn` is keyed by `(row, attempt)`. A retry after a lost reply gets the same
  `seat.started`, never a second agent. `job.run` is keyed the same way. Order ids are already compound-sup's
  at-most-once key.
- **Seat epochs.** The manager numbers each start of a seat. `seat.stop` and `order.deliver` name the epoch and are
  refused against a later one. So a stop that was sent before a respawn cannot end the respawn. Seat ids are minted
  fresh today (`cpd-N`), so this costs one integer and guards the day a seat id is reused.
- **No controller lease yet.** Two daemons on one store (today's `daemon.json` is last-writer-wins) would both be
  accepted. Keys and epochs make their duplicates harmless. The server's leases (state-core 8.2) close it properly.

### 3.4 Transport: network from the start, the local host on the same path

- **TCP, newline-delimited JSON**, the framing that compound-remote's controller and state-core 4.1 already use,
  behind one `Connection` trait so that TLS or a WebSocket is a new implementation and not a new protocol.
- **On the tailnet.** The manager binds its host's tailnet address and loopback, and nothing else. On day one the
  link is encrypted by WireGuard. TLS is added when a host leaves the tailnet (Q5).
- **The local host uses the same path.** wsp on the Mac dials the Mac's manager over TCP, either on loopback or on
  the tailnet address, with the same token and the same code. No unix-socket special case is kept, so "local" is a
  fact about an address and not a code path. On the Mac this costs one TCP connection per CLI call, measured in
  microseconds next to today's 57 ms `ps`.
- **Heartbeats on their own timers.** Each side beats every 10 s on a timer of its own, not on the daemon's tick. A
  side that misses three beats reconnects. This removes the class behind wsp-639, where the beat was paced by a
  78-second tick against a 60-second lease.

---

## 4. What wsp becomes on the other side

### 4.1 The seat state it keeps

wsp keeps the **record**, and the manager keeps the **process**. wsp keeps:
- the seat id, host, row, epoch, kind and model, spawn time;
- the last state, as the manager's events gave it (`spawning`, `started`, `working`, `idle`, `ending`, `gone`, plus
  `lost` when the host is silent);
- the orders and held tells;
- the claim and the binding;
- the last event id heard from each host.

Until state-core step 1 moves state into the store, this lives in today's files (`spawned.json`, `bindings.json`,
`claims.json`, `said.json`). The daemon writes them from events, never from a probe. state-core's seat machine
(3.2) is then a change of where the same record lives.

### 4.2 The inferences it deletes

| inference (state-core 1.6) | deleted from wsp | replaced by |
|---|---|---|
| I1: who owns a process (cwd, descent) | `lsof -d cwd` (check_tree.rs:221, :579; ended_panes.rs:81; cmd_role.rs:671), the `procs.rs` callers, leaked_agents.rs, `leaked-agents.json`, `reap-seen.json` | the manager's seat and job tables |
| I2: is a seat alive, and in what state | `util::alive` per seat, `seat-since.json`, memory.rs's `ps`, waiting.rs's `ps`, herdr's `agent.list` | `turn`, `seat.stopped`, `seats` |
| I3: was an order delivered and taken | screen renders (59.5 per tick), `typed-unsent.json`, `first-orders.json`, the `hooks.jsonl` scans, `stuck-commands.json` | `order.*`, `seat.stuck` |
| I4: has a seat ended | census-by-absence, `finished-wip.json` | `seat.stopped` |
| transcript reads in wsp | agent_commands.rs:2723's callers, did.rs:463, the opencode reads | `seat.said`, `seat.spent`, `seat.did` |

I5 (row state) is state-core's rows machine and is not this plan's.

### 4.3 How the verbs read through the manager

- **`wip`** sends one `seats` request to each host in the machine records, in parallel. Each host's answer is
  `Census::heard(host, seats)`. A host that does not answer within 1 s is `Census::silent(host, why)`, which keeps
  remote-agents' rule that unreachable is never empty (crates/wire/src/place.rs:784, :794, :804). wip runs no `ps`, `lsof`, render or `df`.
  Host facts come from the same replies.
- **`spawn`** sends `seat.spawn` with the first order and waits for `seat.started`, then `order.acked`, by
  subscription. `hand_over` keeps its patience rules and loses its polling.
- **`despawn`** sends `seat.stop` and waits for `seat.stopped`. Its report names the pids the manager ended, not the
  pids wsp guessed.
- **`tell`** sends `order.deliver`. While a host is silent the tell is held (wake.rs:540, unchanged) and flushed when
  the host's connection returns.
- **`watch`** and the wait verbs subscribe to the managers they need and filter on the client.
- **The daemon tick** holds one subscription per manager, folds events into the files, and runs wsp's decisions
  (cycle, ending, closing, attention) over the folded state. These phases go:
  - `tunnels`;
  - `repair:unsent`;
  - `sweep:leaked-agents`;
  - `sweep:ended-panes`;
  - `sweep:check-trees`, once tree jobs are the manager's (M3).

  These phases stay but read the manager's facts rather than probing: `ending:verification` (from `seat.did` digests,
  not transcripts), `sweep:disk` and `sweep:throttle`.

### 4.4 herdr's removal (wsp-120)

D20 makes wsp-120 a line of the first pass. In this plan it is **step M0**, *before* the seam is drawn, for two
reasons:
- A backend that no host runs should not be given a protocol face (D15).
- The 264 references across about 30 files are the shape that keeps bending `Place` (wsp-120's own finding: the
  tests are herdr-shaped).

The removal covers what D20 lists:
- the readers;
- the unreachable warnings;
- the cmd_agent tests that reach a live herdr (wsp-632);
- the panel's herdr focus and install calls (panel/run.rs:481–490, panel/install.rs:259–325);
- "control plane for herdr" in `wsp help`, which still prints today.

After M0 the `Place` implementors are `place_compound`, `place_super` and `fake`, and only those three need a manager
face.

---

## 5. The far machine

The i5 today (remote-agents v1, with wsp-638 and wsp-639 at review):
- one ssh ControlMaster from the Mac's daemon, plus two `-O forward`s per seat;
- `compound-sup` started by a shell line over ssh;
- a beat every 30 s, sent on the tick, through each seat's forwarded controller socket;
- a `compound-remote gate` hook reading the lease file;
- trees copied up and back by rsync with a far manifest;
- the agent's own `wsp` verbs run back over ssh by the shim.

Under this plan the i5 runs **the same manager as the Mac**, installed the same way, and wsp dials it over the
tailnet.

| piece of v1 | becomes |
|---|---|
| ControlMaster and per-seat forwards (tunnel.rs) | Gone. There is one TCP connection per host and no per-seat forward. The 104-byte `SUN_PATH_MAX` concern goes with them. |
| `compound-sup spawn` over ssh (far.rs:488) | `seat.spawn` to the i5's manager, which forks compound-sup locally, inside the GUI session for the keychain token |
| the beat through a forwarded socket, on the tick (far.rs:1124) | The connection's heartbeat on its own 10 s timer. On each beat from a wsp holder the manager writes `<exec_dir>/lease/<seat>` `ok` for each seat whose claim wsp says is held, and `stop` for one that is released. The gate is unchanged and still **fails closed**. wsp-639's flap cannot happen: the beat is no longer paced by the tick. |
| the lease, a machine field (default 180 s) | Unchanged in meaning. The manager reads it from `hello`. |
| `machine check`: wire, a throwaway session, `claude -p` | `hello` reports the wire, and a `check` request runs the session on the host. |
| the far manifest and copy (treecopy.rs) | Under T1 the far half runs inside the manager, hashing in-process (wsp-638's 4 min 25 s was a fork per file). The bytes still go by rsync over ssh, or by a `tree.copy` stream in the protocol (Q7). Under T2 it is retired. |
| the far verifier (remote-agents 11.3) | A seat like any other on the i5, with its checks run as the i5 manager's jobs. |
| unreachable, `despawn --lost`, `machine retire` | Unchanged. A host is silent when its connection has missed three beats past the lease, and its seats read `lost`. |

**What of the ssh door stays:**
1. **Install and upgrade**: copying binaries, the plist and the token, and `launchctl` on the far side. The manager
   cannot install itself.
2. **The tree bytes under T1**, unless Q7 moves them into the protocol.
3. **The agent's own `wsp` calls** (the shim: `say`, `note`, `review`, `submit`, the SessionStart brief). These are
   calls *to* wsp's state, and the state is on the Mac until the server. The recommendation is to keep the shim's ssh
   until state-core step 1 gives the agent a network API. The alternative is to relay them through the manager's
   connection. That needs a wsp process on the Mac to run each relayed verb, which is the daemon running CLI verbs on
   behalf of agents (Q8).
4. **A person's debugging**, which ssh always serves.

The far-only fields `exec_dir` and `wire` stay on the machine record. `transport` gains the value `manager` next to
`ssh` (remote-agents §4: "the machine record names its transport").

---

## 6. Migration

### 6.1 Baselines

From `wsp calls --since 24h`, 2026-11-16T06:48Z.

**Per tick** (1,633 ticks): 37.5 `ps`, 59.5 `compound-render`, 24.6 `df`, 0.9 `lsof`, 1.3 `kill`. The tick's p50 is
19.5 s and its p95 116 s.

**Per verb:**

| verb | measure |
|---|---|
| `wip` | 464 calls, 1,360 CPU-s (2.9 CPU-s per call), p50 3.5 s |
| `wip --json` | p50 2.7 s |
| `spawn` | **p50 9.4 s**, p95 35.7 s |
| `despawn` | p50 2.9 s |
| `tell` | p50 0.86 s |

**compound's host polling** (`none via host`): 28,032 calls and 25,268 CPU-s, 17.2% of the total, with `ps` 41,459×,
`compound-render` 195,490× and `lsof` 2,227×. **ssh**: 147 runs, p95 38 s.

**Incidents.**
- **wsp-478 kind** (a process stopped for standing in a tree it did not start): cycle.log lines `stopped in finished
  row … TERM` whose pids were not started by that row.
- **wsp-411 kind**, which is state-core 1.6's I4: a seat's end inferred from absence, such as a second idle seat
  after a re-plan. Row wsp-411 itself is the wire cut, and state-core files this incident class under it. Counted as
  seats reaped by a census that later answered.

`wsp calls` records costs, not incidents, so M0 adds an `--incidents` view over cycle.log and events.jsonl that
counts both kinds. The before figure is taken from it.

### 6.2 The steps

Each step lands behind the previous one, and the live fleet runs on every intermediate state. Each step's measure is
taken with `wsp calls` over a day before and a day after, and its incident counts over a week.

| step | what | measure: done when | rollback |
|---|---|---|---|
| **M0. herdr out, seam named** | wsp-120 (4.4). `host-api` holds the types of 3.2. Every OS-touching call outside tree work goes through one `Host` trait (`crates/wire/src/host.rs`, its methods grouped by the messages of 3.2). Its only implementation is **in-process** (`host::InProcess`): today's code, behind the trait. The seat operations of place_compound.rs and place_super.rs are behind it first (wsp-005), then the transcript and hook-log reads, grouped as `seat.did` and `turn` (wsp-007), and the host facts (wsp-006): `df`/`du`, per-seat memory and running shells (`ps`), the throttle's `sysctl`, `pmset`, `vm_stat`, `memory_pressure` and `ioreg`, pid liveness and a check run's `lstart`. Layers below wire (base, store) ask nothing themselves: their callers pass the host's answer in. `wsp calls --incidents`. | `grep` finds no `ps`, `lsof`, `kill`, render, transcript or fact probe outside the in-process host (tree work excepted). Spawn p50 ≤ 9.4 s, `wip` CPU no worse, tick no slower. Herdr references → 0. | Revert. It is a pure move plus deletions, behind the same tests. |
| **M1. The manager on the Mac** | The in-process host becomes the `wsp-manager` process under launchd. wsp dials it on TCP (3.4). Hooks report to it, and the journal and subscription are in use. The daemon folds events. `wip` reads `seats`. compound's host keeps polling `wip --json`, which is now cheap. | **ps** per tick 37.5 → ≤ 1. **renders** per tick 59.5 → 0. **`df`** 24.6 → 0. **lsof** in `wip` → 0. `wip` < 0.2 CPU-s per call (from 2.9). Host polling CPU-s per day 25,268 → under 2,500. Spawn p50 no worse than 9.4 s. wsp-411-kind incidents → 0 over a week. A manager kill -9 mid-turn loses no seat and no order (a scripted proof, 10×). | `WSP_HOST=inproc`: the same code linked into wsp, as at M0. Seats started by the manager stay alive (compound-sup is durable), and the in-process host adopts them by socket. `WSP_HOST` is `manager` when unset (wsp-029); a manager's start adopts the seats the in-process host started, from compound's run directory. |
| **M2. The i5 under its own manager** | The same binary on the i5. wsp dials it over the tailnet. The beat becomes the heartbeat. Per-seat forwards and tunnel.rs's masters go, and the far half of the copy moves into the manager. | **ssh** runs per day 147 → install and tree bytes only. A far seat working through a slowed tick never reads unreachable (a wsp-639 sandbox reproduction plus a week live). The far manifest of compounding's 8,788 files < 15 s (wsp-638's 4 min 25 s). `spawn --on` p50 no worse. | A machine field `transport: ssh`, which keeps the v1 path compiled until a fortnight passes on `manager`. |
| **M3. Tree jobs under the manager** | `throttle run`, `check`, `verify`'s checks and barrier runs become `job.run` on the tree's host (the Mac). The throttle admits by the manager's facts. check_tree's sweep, the slot probes and leaked_agents go. | **lsof** → 0 and **ps** → 0 on the wsp side. Every `kill` is by a manager, on a pgid it started. wsp-478-kind incidents → 0 over a week. `throttle run`'s wall per build no worse. | Jobs fall back to wsp running them in-process behind a flag. The sweep is restored with them. |
| **M4. Trees placed (if Q6 says T2)** | `tree.checkout` and `tree.land` on every host's manager, a fenced push to the trunk, per-host warm targets, and the barrier's host named. | A list runs with members on two hosts and no copy-up or copy-back. Land conflicts are refused by the push and never overwritten. Cold-build cost on the i5 is measured against remote-agents' early proof. | Rows placed on the Mac only (T1), which is the state at M3. |

Spawn p50 is the guard rail at every step: a step that makes it worse than 9.4 s, measured the same way, does not
finish.

#### M0's result (wsp-009, 2026-11-17)

**The guard.** `tests/host_seam.rs` is the done-when's `grep`, made a test. It reads the product code of `src/` and
`crates/*/src/` with a small lexer: string literals are kept, and comments, `#[cfg(test)]` items, `test-support`
items and files declared as test modules are left out. It fails when any of these is reached outside
`crates/wire/src/host.rs`:
- `ps`, `pgrep`, `lsof`, `kill`, `pkill`, `killall`, run as a `Command`, written into a shell line, or declared as a C
  function;
- the fact probes `df`, `du`, `sysctl`, `pmset`, `vm_stat`, `memory_pressure` and `ioreg`, the same three ways;
- running `compound-render`, or naming `host::compound_render_binary` or the helpers that type through it;
- building the path of a Claude Code transcript (`~/.claude/projects`) or a pi session, naming `hooks.jsonl`, or
  asking `opencode` for its `export` or `db`;
- calling or importing one of host.rs's own OS-touching free functions (`signal_group`, `tree_of`, `tally_burn`,
  …), which reaches the OS past the trait.

A second test fails when "herdr" appears anywhere under `src/` or `crates/`, tests and comments included. A third test
checks the guard's reading on a fragment of each kind.

What the code still does outside the host is listed in `docs/host-seam-allowed.txt`, with the reason for each line.
That file is a ratchet like `docs/components-upward.txt`: a new hit fails, a line the code no longer has fails, and a
line the trunk's copy lacks fails. It has four lines:
- `far.rs` (2): `DISPLAY_PROBE` (`ioreg`) and `holdings_command` (`ps`) are shell lines run *on the far machine* over
  ssh. host.rs leaves the far machine to M2, and these two go with it.
- `warm.rs`: `remove_when_gone` waits on its own pid (`while kill -0`) before an `rm`. That is tree work (1.3), and
  it becomes a job at M3.
- `did.rs`: `is_scratch` names `/.claude/projects/` to classify a path that a tool call touched. It reads nothing.

Writing the guard turned up one leak, and it was fixed: `place_super` re-exported `signal_group` and `tree_of` to the
whole crate, and only tests used them. The re-export is now `test-support` only.

The guard does not catch two things. A file read of a path handed in from elsewhere looks like any other read, so
the guard watches where transcript paths are *built*, not every `fs::read`. And disk.rs's `private_size` walk (the
size of wsp's own state, through `getattrlist`) is an in-process file walk, not a probe.

**Incidents.** `wsp calls --incidents [--since 7d] [--json]` counts the `cycle.log` entries (a dated line with its
continuation lines) that the daemon writes when it acts on one of state-core 1.6's inferences, by kind. A test holds
each sign to its writer's own words. It also counts 6.1's two classes:
- **wsp-478's**: `stopped in finished row … TERM`. All of them are counted. Whether the pids were the row's own
  cannot be told after the fact, so this is an upper bound.
- **wsp-411's**: a seat ended on a census (the `nothing left to do, ended:` sweeps, `ended — …, and no pid of it is
  running`, and I4's "already gone") that is heard from afterwards. "Heard from" means an event it caused in
  `events.jsonl` (`pane` or `by` is the seat; wsp's own `agent-told` does not count) or a line read off its own hooks
  (`<seat>: …`).

The before figure, over the 7 days to 2026-11-17T10:06Z on this store:

| kind | count | what the signs were |
|---|---|---|
| I1 who owns a process | 0 | |
| I2 is a seat alive | 14 | `never became ready for input` (the readiness read off the screen at spawn) |
| I3 was an order taken | 8 | `but its pane did not get it` |
| I4 has a seat ended | 2 | an accepted exit's seat already gone |
| I5 row state | — | never logged as a guess (state-core's rows machine) |
| **wsp-478 kind** | **0** | |
| **wsp-411 kind** | **0 of 30** | seats ended on a census, none heard from afterwards |

**The measures, and how they were taken.** The live daemon and every seat run the installed binary, `4be352b` (this
plan's own commit), which is from before M0. So every figure the fleet records today is a before figure. The after
figure was taken as an A/B on this machine at 2026-11-17T10:03Z, under a load average of 7.9–9.4. Two release builds
were compared: `4be352b` (built from `git archive` into the row's scratch directory) and this tree (`90d47e3`, the
M0 trunk plus this row, built before its rebase onto wsp-012's comment-only commits). They ran alternately, with `WSP_NO_CALLS=1` so that the fleet's `calls.jsonl` was not
touched. CPU is user+sys of the process and every child it waited for (`wait4`). The script is `ab.py` and
`spawn_ab.sh` in the row's scratch directory.

| measure | 6.1 baseline | live, `4be352b`, 14 h to 2026-11-17T10:01Z | A/B before (`4be352b`) | A/B after (M0) |
|---|---|---|---|---|
| `wip` CPU per call | 2.9 CPU-s | 0.15 (actions), 0.21 (polls) | 0.131 CPU-s p50, n=15 | 0.131 CPU-s p50 |
| `wip --json` CPU per call | — | 0.22 (polls) | 0.133 CPU-s p50, n=15 | 0.132 CPU-s p50 |
| `wip` wall p50 | 3.5 s | 0.16 s (actions), 0.20 s (polls) | 0.142 s | 0.142 s |
| `spawn` p50 | **9.4 s** | **2.09 s** (62 spawns), p95 68.9 s | 0.800 s, n=12 (workspace only) | 0.800 s (workspace only) |
| `despawn` p50 | 2.9 s | 0.08 s | 0.026 s | 0.026 s |
| tick p50 / p95 | 19.5 s / 116 s | 85 ms / 1.60 s (974 ticks) | — | — |
| per tick: ps, render, df, lsof, kill | 37.5, 59.5, 24.6, 0.9, 1.3 | 5.8, 3.6, 0.3, 0.6, 0.3 | — | — |

How each was taken:
- **The live columns** are `wsp calls --since 24h` and `--polls` on this store. The log covers 14 h, since the daemon
  started at 2026-11-16T19:55Z. The tick and child figures are the same file's `"k":"tick"` lines.
- **`wip`** was run against the live store with both binaries. With 2 seats up, it is a light fleet.
- **`spawn`** is `wsp spawn <row>` *without* `--agent`, then `wsp despawn`, in an isolated `WSP_HOME`/`WSP_STATE`
  per binary with a one-commit repo. It times what M0 moved (the tree, `compound-sup spawn` through `Host`, the
  claim) and leaves out claude's own start-up, which M0 does not touch. Spawning claude twelve times per binary would
  have bought a figure dominated by the agent's own readiness. The live column carries the agent.
- **6.1's baselines** come from a heavier day than this store has seen since (1,633 ticks, tick p50 19.5 s). They
  bound the result but are not its comparison. The comparison is the A/B, on one machine at one moment.

**Against the done-when line:**
- *`grep` finds no `ps`, `lsof`, `kill`, render, transcript or fact probe outside the in-process host (tree work
  excepted)*: **met, with two named exceptions.** The guard holds. Its allowances are tree work (`warm.rs`, as the
  line excepts), a path classifier that reads nothing (`did.rs`), and **far.rs's two ssh shell lines**. Those two are
  not tree work. They are left to M2, which removes the ssh door they run through.
- *Spawn p50 ≤ 9.4 s*: **met.** It is 2.09 s live, and the A/B shows the same p50 before and after.
- *`wip` CPU no worse*: **met.** 0.131 CPU-s per call before and after.
- *Tick no slower*: **not measured on an after binary. This is the shortfall.** There is no single-tick mode. A tick
  run on a copy of the live state would act on the live seats (spawn agents, signal recorded pids), and the daemon
  runs `4be352b`. M0 changes no tick logic, only where the calls are made. But the day-after `wsp calls --since 24h`
  that 6.2 asks for is owed once the M0 trunk is installed (`wsp install`). That is the person's step, not a seat's.
- *Herdr references → 0*: **met.** `herdr_is_gone_from_src_and_crates` holds.

**Carried into M1's breakdown, first:** wsp-010. It covers the features herdr fed that nothing feeds now: overlap's
live panes and verify's `--rm --all` sweep.

*wsp-010, decided:* both are re-fed, neither deleted. Overlap's `World::live` takes its panes from compound's census
(`overlap::panes_of`: a seat that has gone is not standing, a shell with no agent still is). Verify's `--rm --all`
residue sweep takes its live keys from the same census (`cmd_verify::live_keys`: the held seats, and `solo`); a census
that did not answer removes nothing. Tests: `a_live_seat_in_the_census_is_a_pane_standing_beside_us` and
`the_residue_sweep_is_fed_by_the_seats_compound_holds`. Left for the far-seat paths wsp-001 named (the `machine`
reads): they still treat every seat as local.

#### M1's breakdown (wsp-019, 2026-11-17)

The list is `m1-agent-manager`, at status init. It follows wsp-018's three rules: expand, then contract; a cut that
cannot be staged is one row run as legs; every row's checks build and test every crate it touches.

| group | kind | rows | what |
|---|---|---|---|
| 1 | contract | wsp-020, wsp-010 | The in-process host's OS code moves into a store-free crate, `crates/host` (`wsp-host`), which wire re-exports. wsp-010 re-feeds or deletes what herdr fed. |
| 2 | components | wsp-021 | `crates/manager` (`wsp-manager`): TCP, `hello` and its token, the journal and `subscribe {from}`, the beat. No seats yet. |
| 3 | contract | wsp-022, wsp-023, wsp-024 | The manager serves seats and orders, with keys, epochs and adoption on restart. Hooks report to it. `wsp install` puts it under launchd. |
| 4 | wiring | wsp-025 | `host::Tcp`, the `Host` over the connection, behind `WSP_HOST=inproc\|manager`, with the default left at `inproc`. |
| 5 | wiring | wsp-026, wsp-027, wsp-028 | Callers move under `manager`: spawn, despawn and tell; `wip` reads `seats`; the daemon subscribes and folds (as legs). |
| 6 | contract | wsp-029 | `WSP_HOST` defaults to `manager`, and seats are adopted both ways. Stop: a live day on the installed trunk. |
| 7 | contract | wsp-030 | Contract: wsp deletes the polls, sweeps and inference files the events replaced, as legs. Stop: install, then a day. |
| 8 | insitu | wsp-031 | The M1 line's measure and the scripted `kill -9` proof, written here as "M1's result". |

host-api already holds 3.2's types and the `Connection` trait (wsp-002), so no group changes the contract crate. A
gap found on the way is a row of its own (components.md R5).

#### Group 1: the in-process host in its own crate (wsp-020, 2026-11-17)

The in-process host is now `crates/host` (`wsp-host`). It depends on `wsp-base` and nothing else of the
workspace, and `tests/layering.rs` (`the_host_crate_links_neither_the_store_nor_the_wire`) walks its path
dependencies to keep it that way. It holds:
- `InProcess`: its methods are the old `impl Host for InProcess`, with the same names.
- The free functions those methods are built from. These are every free function the guard names.
- What they needed of the wire, moved down:
  - `place`'s `Seat`, `State`, `Refusal`, `Delivery` and `Result`, and the shed list.
  - `place_super`'s prices, `read_json` and `str_of`.
  - `place_compound`'s `WIRE_VERSION`, `DELIVERY_PENDING`, `reads_kind` and `test_sups`.
- The store's `checks::ps_says`, which went to `wsp-base`'s `procs` instead.

The wire re-exports each item at its old path, so no caller changed. `crates/wire/src/host.rs` is the adapter.
It keeps the `Host` trait and `LOCAL`, plus a `host::InProcess` that wraps `wsp_host::InProcess`, with each
trait method calling the in-process method of the same name. It is a newtype rather than the type itself, so
a caller still names the trait to ask the host anything. `host::Tcp` (group 4) stands beside it. Only the
host's `write_atomic` is new code: it is the store's, without the write counter of the store's `steady`
passes. Those passes never walk a seat's directory. The guard (`tests/host_seam.rs`) now allows the probes
under `crates/host/src/` and nowhere else. Run with the exclusion lifted, it finds all of them (16 hits) in
`crates/host/src/inproc.rs`, and none in the wire. `host-api` is not a dependency yet. It joins when the
manager's messages are spoken here (group 2 and after).

#### Group 2: the manager process (wsp-021, 2026-11-17)

`crates/manager` (`wsp-manager`) depends on `wsp-host-api`, `wsp-base` and `wsp-host`, and on nothing of the
store. `tests/layering.rs` (`the_manager_links_no_store_and_no_wire`) holds that rule. `wsp-host` gained two things
that the manager and wire's client will share:
- `Tcp`, the TCP carrier of `Connection` (3.4): newline-delimited JSON, a 16 MB line cap, and a write timeout of
  three beats;
- `facts`, the beat's `HostFacts`, read in three asks (one `sysctl` for load, swap and the kernel's memory
  pressure level, one `df` for every volume, and `pmset` for the thermal state).

`wsp-host` now depends on `wsp-host-api`. Nothing in wsp dials the manager yet.

What the process does, as section 2 and section 3 ask:
- **It listens on loopback and the tailnet's addresses, and nothing else.** `allowed` admits `127.0.0.0/8`, `::1`,
  `100.64.0.0/10` and `fd7a:115c:a1e0::/48`, and refuses anything else before it creates a file. The tailnet
  addresses come from `tailscale ip`, or from `--tailnet ADDR`. Without either, the manager serves on loopback
  alone and logs that it does. The default port is 7487.
- **It makes its token at first start.** The token is `<exec_dir>/manager.token`, 32 random bytes as hex, mode
  0600. A token file that was widened is narrowed back to 0600 and logged. A `hello` with the wrong token is
  answered `refused unauthorised` and the connection is closed. So is a first frame that is not a `hello`.
- **It answers `hello`** with the host id, version, protocol, compound-sup and compound-render wire range
  (`WIRE_VERSION` both ways), journal head, seats (none) and capacity. Capacity is 0/0/0 until it runs seats and
  jobs. The last beat is queued straight after the `hello`, so a one-shot call has the host's facts without
  waiting out a period.
- **It journals every event before sending it.** `<exec_dir>/journal.jsonl` holds one frame per line, with ids
  that run from 1 per manager. A torn last line is cut at open. `subscribe {from}` replays every event after
  `from` and then follows live. The journal's mutex is held across both the append-and-send and the
  replay-and-register, so no event is sent twice or falls between them. `Manager::emit` is the one way in, and
  its callers are wsp-022's seat operations.
- **It beats every 10 s on its own timer.** The facts are read once per beat for all clients. A fact the host does
  not answer is sent at its quiet value and logged, because `HostFacts` has no "unknown". A client that is
  silent for three beats is dropped.
- **It refuses to start while another manager answers its port.** It first dials its own port with its own token,
  and names whatever answers: a manager, a manager with another token, or something else. A bind that finds the
  port in use is refused the same way. It also refuses while another manager holds `<exec_dir>/manager.lock`
  (`flock`), since two managers on one journal would both number events.
- **It logs to `<exec_dir>/log/manager.log`**, rotated once past 10 MB.

Seat verbs are answered rather than ignored: `seat.spawn` gets `seat.refused unsupported`, `order.*` gets
`order.refused no_seat`, `seat.stop` and `seat.read` get `refused no_seat`, and `job.*` and `tree.*` get
`refused unsupported`.

Tests (`cargo test -p wsp-manager`, `tests/manager.rs`) run a manager on an ephemeral loopback port. They check
that:
- a `hello` with no token, or the wrong one, is closed, and one with the token is answered;
- reconnecting `from` N gives exactly the events after N, also across a restart on the same port;
- a second manager refuses both the port and the `<exec_dir>`;
- the beat carries the facts;
- a client silent for three beats is dropped.

The binary was also run by hand on loopback. Its `hello` and its beat, with this Mac's load, memory, swap, disk
and thermal state, came back to a Python client.

#### Group 3: the manager serves seats and orders (wsp-022, 2026-11-17)

`crates/manager/src/seats.rs` holds the seat table. The manager now answers 3.2's seat messages over `wsp-host`'s
in-process code, instead of refusing them.
- **`seat.spawn`, keyed by `(row, attempt)`.** The manager forks `compound-sup spawn --label <seat>` itself, with
  `COMPOUND_RUN_DIR` set to `<exec_dir>/run`, so every seat's sockets and `.label` sidecar sit under the manager's
  own directory. A retry of the key gets the same `seat.started`. A spawn that arrives while the key is still
  forking waits for that same answer, so one key never starts two agents. These are refused `duplicate`: a key
  whose seat already stopped, a key running another seat or other arguments, and a seat id already running under
  another key. `gate: required` is refused `unsupported`, because local seats are not gated (d1 Q12). A `tree`
  workdir is refused `no_tree` until Q6 places trees on hosts. The agent's env names are filled from the
  manager's own environment.
- **`seat.stop`** signals the session's process group (`TERM`, then `KILL` after `grace`). It answers
  `seat.stopped` with the pids of the seat's tree that ended. A supervisor the manager is parent of and that
  exits unasked is reported `seat.stopped` with its status.
- **Seat epochs.** There is one epoch per start of a seat id. Epochs are counted from the journal, so they run on
  across a restart. A `seat.stop` or `order.deliver` naming an older epoch is refused `stale_epoch`, with the
  seat's epoch in the reason.
- **Orders go through `compound-sup deliver <socket> <id>`.** The order is written to `<exec_dir>/orders/`
  before compound-sup is asked, and the file is removed only after the answer is journalled. A `delivered`
  answer becomes `order.delivered` and `order.acked`; compound-sup says `delivered` only once the agent's
  `UserPromptSubmit` hook has said the turn began. A `failed <why>` answer becomes `order.refused` with
  compound-sup's `FailWhy`. An order asked again gets the same answer and is never typed a second time. The
  ack's `turn_began` is the time the manager read the answer.
- **One owner of `turn` (wsp-032).** The hook fold (wsp-023) journals every `turn`, began on `UserPromptSubmit`
  and ended on `Stop`; the order delivery journals none. Before compound-sup is asked, the delivery tells the
  fold that the seat's next turn is this order's, so the `turn` began carries the order id; a refused order is
  forgotten. A seat whose hook could not reach the manager (it fell back to `hooks.jsonl`) gets no `turn` from
  the manager, which M1 accepts.
- **`seats`** takes one `ps` for every seat's rss and shells, plus a `compound-sup vitals` per seat, and renders
  no screen. **`seat.read`** is the one render.
- **Restart (2.5).** At start the table is rebuilt from the `*.sock` files in `<exec_dir>/run`. Each socket is
  joined by its `.label` to the journal's last `seat.started` for that seat, and kept if `vitals` answers. A seat
  the journal has running but whose session is gone is reported `seat.stopped`. An order left in
  `<exec_dir>/orders/` is asked again under its id, unless the journal already holds its answer.
- **Capacity** is `--seats`, 32 by default. A spawn past it is refused `full`.

Tests (`tests/seats.rs`) use a fake compound-sup: a shell script with plain files for sockets and a sleeping
process group for each session. Nothing spawns claude. They check that:
- two spawns with one key give one seat;
- a stop or an order naming a stale epoch is refused;
- an order answers delivered, then acked;
- the `wsp-manager` binary, killed with SIGKILL mid-order and restarted, still lists the seat, answers the order
  once (typed once), and resumes events `from` the last id.

#### Group 4: `host::Tcp`, the `Host` over the connection (wsp-025, 2026-11-17)

`crates/wire/src/host_tcp.rs` holds the wire's second `Host`, `host::Tcp`. It dials the manager that the local
machine record names (`<state>/manager/local.json` and its secret file, wsp-024) over `wsp-host`'s `Tcp`.
- **The moved methods become messages:**
  - `spawn_session` → `seat.spawn`;
  - `stop` and `stop_soon` → `seat.stop`, for a seat this process started through it;
  - `deliver` → `order.deliver`, for a seat the manager runs. A refusal is worded as the in-process host words
    compound-sup's (`Failed("<seat>: busy")`, `NoSeat`);
  - `read_screen` → `seat.read`;
  - `can_deliver` and `wire_health` → `hello`;
  - `Tcp::seats` asks `seats`.
- **Everything else stays in-process under both choices:**
  - the process table, signals and working directories (tree work's until M3);
  - hook files and transcripts (the manager reports them as events, which group 5 folds);
  - the fact probes' text (the beat carries `HostFacts`, not text);
  - headless seats, typing at a renderer, and any seat the manager does not run (one started in-process is
    adopted at the manager's next start, wsp-029).
- **One connection per process (3.4).** It opens at the first message, and is reopened once if the manager
  closed it. The client beats during a long wait.
- **Heard or silent.** A manager that does not answer `hello` within 1 s is `HostError::Silent`, whose message
  begins "host silent". Through the trait it is `Refusal::Unreachable`, never an empty answer.
- **`WSP_HOST=inproc|manager`.** `host::chosen` reads it, and `host::choose` takes the choice as an argument.
  The default stayed `inproc` until wsp-029 made it `manager`. Nothing calls them yet: callers move in group 5.
- **The spawn's key is real; its env is the 3.2 rule.** `Spawn` carries `key: (row, attempt)`, and `Tcp`
  sends it as is, built once per spawn so `ask`'s one reopen resends the same key. wsp builds it in
  `cmd_spawn::spawn_key` from the row's id and the count of its log's "started by pane" lines, puts it on
  `place::Agent`, and `Compound::start` hands it down. `start_agent`'s own re-start after a start that did
  not survive takes the next attempt, because the manager refuses a stopped key as `duplicate`. A seat with
  no row (a bare-shell `open`, a rotation that is not a task) has no key to hand down, so the host keys it
  `(seat, 0)`: wsp mints seat ids fresh, so nothing else can share it. The seat's env values still cross
  as an `env -u K -- K=V` prefix on the argv: 3.2's rule is that the prefix carries wsp's per-seat values
  (row, store) and never a secret.
- **`host::Subscription`** sends `subscribe {from}` with the last event id heard of that host, kept in
  `<state>/manager/<host>.last`. It hands out events and beats one at a time. An id is kept once the next is
  asked for, so delivery is at least once. A dropped connection, or three silent beats, means it subscribes
  again from that id. A replay that overlaps the kept id is skipped.

Tests (`cargo test -p wsp-wire --test host`) hold `InProcess` and `Tcp` to one contract with the same fake
compound-sup. The `Tcp` runs on a loopback manager. The contract checks that:
- a spawn gives a session;
- an order is acked, and asked again under its id it is typed once;
- `busy` is refused as in-process;
- a stop ends the group;
- an order to a gone seat is `NoSeat`.

They also check that `WSP_HOST=manager` against a closed port, or a listener that never answers, is "host
silent" within the second, and that the subscription resumes from the last id across a manager restart and in
a new subscription. `tests/host_seam.rs` holds: `host_tcp` reaches no OS itself.

#### Group 5: `wip` reads `seats` (wsp-027, 2026-11-17)

Under `WSP_HOST=manager`, `wip` is `Wip::from_managers` (src/cmd_agent.rs). Under `inproc` it is `Wip::live`,
unchanged. Both share the store half (`Wip::gathered`); they differ only in what they ask the host.
- **The census.** `host_tcp::census` sends one `seats` to each manager the machine records name, in parallel.
  Today that is the local record alone, filed as `""`. A host that answers within 1 s, dial and `hello`
  included, is `Census::heard`. Any other is `Census::silent` with the "host silent" reason. A seat's label,
  directory and agent come from compound's record files (`Compound::seated_as`). Its state is the manager's:
  `stopping` reads `gone`.
- **Host facts from the same reply.** `seats.list` gains `host`, the facts of the manager's last beat. The field
  is optional both ways, so an older manager still answers.
- **Read from the replies:**
  - an idle member's shells are `shells` (`Shells::counted`, a count alone);
  - idle memory is `rss_bytes` (`memory::of_rss`);
  - a low disk is the facts' `disks` (`disk::loud_of`);
  - the dead-daemon heading is `cycle.log` quiet past a minute, with no `ps`.
- **Not run under the manager:**
  - the finished-tree scan (`wip` draws the daemon's last `finished-wip.json`);
  - the ended-panes scan (the daemon closes those).
- **Silent is never empty.** A manager that did not answer is said in `wip`'s `unreachable` heading, which is
  the json's existing `unreachable` key, as "this machine's manager (host silent: …) — its seats are not read,
  not gone".
- **Left for later:**
  - the throttle line still reads load and thermal in process (`sysctl`, `pmset`), because the facts carry
    neither the 5-minute load nor the speed-limit percent it words;
  - a seat started in-process is not in the manager's answer until the manager's next start adopts it
    (wsp-029);
  - the manager's `shells` count does not apply `Shells`' settled, work and quiet bounds.

`tests/wip_manager.rs` runs the `wsp` binary's `wip --json` against a loopback manager with one seat (fake
compound-sup). Under `manager`, the call log's children hold no `ps`, `lsof`, `df` or `compound-render`, and
the seat is listed. Under `inproc`, the same call runs `ps`. The json's keys are the same under both. With the
manager stopped, `unreachable` names it as host silent, still with none of those children.

**Group 5, spawn (wsp-026).** `wsp spawn` builds compound through `Compound::chosen`, whose host is
`host::chosen`'s. A silent manager is the spawn's error ("host silent"), never the in-process host in its place.
- A work order that is told rather than put in argv goes in `seat.spawn` as `first_order`, under an id minted per
  start (`place::new_order_id`). It goes through `Place::start_ordered` → `Host::spawn_ordered`.
- `Tcp` subscribes from the journal's head first (`Subscription::at_head`, which keeps its place in memory and
  never in the daemon's `.last`). Then it sends the spawn and reads `seat.started` or `seat.refused` as the
  request's answer, because `seat.refused` is not journalled. After that it listens on the subscription for the
  order's `order.acked` or `order.refused`, or for the seat's `seat.stopped`.
- `cmd_spawn::start_ordered` keeps `Patience`:
  - `ready + taken` is the wait for the seat and its ack;
  - `nudges` are presses on an unanswered order, each followed by `taken` more (`Place::await_order`);
  - `attempts` retries a refused seat under the next attempt's key;
  - a silent host is said, not retried.
- None of this polls `state` or reads a screen. Under `inproc`, `start_ordered` is `None` and the old start,
  `wait_ready` and `hand_over` run unchanged.

#### Group 5: spawn, despawn and tell through the manager (wsp-026, 2026-11-17)

Under `WSP_HOST=manager`, `spawn`, `despawn` and `tell` go through the manager's messages and events. Under `inproc`
nothing changes. The old code is deleted later, by the contract row.
- **spawn** sends `seat.spawn` with the first order and waits by subscription for `seat.started`, then
  `order.acked`. It makes no `state` poll and no render. A silent manager is "host silent", never the in-process
  host in its place.
- **despawn** sends `seat.stop` and waits for `seat.stopped`. Its report names the pids the manager ended
  (`Host::end` returns `Ended{pids, exit}`), and the json adds `ended_pids` only then. A seat the manager does not
  run, or a silent manager, is said plainly; the seat's record stays.
- **tell** sends `order.deliver`. A silent manager, or an unreachable census, holds the tell (wake.rs's hold,
  unchanged). The flush delivers it once the manager answers, and the order id kept beside the hold means a re-ask
  is typed once.
- **The key** is the real `(row, attempt)` (group 4): the same key on a retry after a lost reply, the next attempt
  when a start did not survive.

Tests:
- `crates/wire/tests/host.rs`, on a loopback manager with the fake compound-sup:
  `spawns_under_one_key_over_tcp_give_one_seat_and_another_attempt_another`,
  `a_spawn_under_manager_carries_its_order_and_hears_it_acked_with_no_poll_and_no_render`,
  `a_stop_under_manager_reports_the_pids_seat_stopped_names`,
  `a_tell_under_manager_is_delivered_and_a_silent_manager_is_unreachable_then_delivered_on_return`.
- `cmd_spawn::tests::despawn_under_a_manager_asks_the_manager_and_a_silent_one_keeps_the_claim`.
- `cmd_agent::tests::a_tell_to_a_silent_manager_is_held_and_the_flush_keeps_it`.

#### M3's breakdown (wsp-042, 2026-11-17)

The list is `m3-agent-manager`, at status init. It is planned on top of M1 alone, because no far machine is registered and M2 is not available. It follows wsp-018's three rules:
- expand, then contract. A job runner is added beside today's in-process runs, behind `WSP_JOBS=inproc|manager`, which defaults to `inproc`. Callers move one area at a time. The old spawns and the sweeps are deleted last;
- a removal that cannot be staged is one row run as legs;
- every row's checks build the workspace and test every crate it touches, with at least 120 s per cargo test step.

Each row's overview names the files it changes.

| group | kind | rows | what |
|---|---|---|---|
| 1 | contract | wsp-044 | host-api completes the job contract, which a contract row must do (R5). It adds where a job runs (a place on the tree's host, never a path on wsp's machine), keys unique per run (wsp-040's lesson), `hung_after_secs`, the env prefix rule of 3.2, `job.output`, a `jobs` request, `detach` (an undetached job ends with its connection), and the throttle's facts on the beat (d1 Q11). |
| 2 | components | wsp-049 | `crates/host/src/jobs.rs`, the runner that both the manager and the in-process host link. Each job gets its own process group, a log, timeout and hung bounds, the group killed on every end (wsp-453), an exit file for an end nobody heard, and re-adoption by pid and start time. facts.rs reads the new facts. |
| 3 | components | wsp-050 | The manager serves `job.*`: keyed runs, a hard cap per class, events journalled, `job.stop` on its own pgid only, and re-adoption on restart. |
| 4 | wiring | wsp-051 | `Host::run_job`, `stop_job` and `jobs` on `host::Tcp` (waiting by subscription, never by a poll) and on `InProcess`. `WSP_JOBS` is independent of `WSP_HOST`. |
| 5 | wiring | wsp-052, wsp-053, wsp-054, wsp-055 | Callers move under `manager`, one row per area: `throttle run` (its output streamed, ^C as `job.stop`, slots from `jobs`, admission from the beat); `check`'s `run_step` and its locks; `verify`'s cargo, listing and `--alone`; barrier runs and warm.rs's cleanup. The barrier row rests on check's. |
| 6 | contract | wsp-056 | `WSP_JOBS` defaults to `manager`, and `wsp install` writes the job capacities. Stop: a live day on the installed trunk. |
| 7 | contract | wsp-057 | Contract, as legs: delete check_tree's sweep, leaked_agents, the slot probes, the direct spawns of tree work, Host methods left with no caller, and warm.rs's allowed line. host_seam loses "tree work excepted". Stop: install, then a day. |
| 8 | insitu | wsp-058 | The M3 line's measure, written here as "M3's result": `lsof` and `ps` at 0 on the wsp side, every kill matched to a pgid the manager started, wsp-478-kind incidents at 0 over a week, `throttle run`'s wall no worse, and spawn p50 no worse than 9.4 s. |

What M1's live defects taught, carried into the rows:
- **A caller that goes around the seam.** In M1 the governor's reseat and rotation did this (wsp-036, wsp-041). Every caller row is asked to `grep` for all callers, and the contract row makes that a guard test.
- **A key reused after a reopen** (wsp-040). Job keys are unique per run.
- **Liveness judged from a state read that is not yet true** (wsp-037). Jobs are waited on by subscription.

**Assumes M2:** nothing M3 builds. Rows placed on the i5 keep today's ssh path, and their builds and checks keep running in-process there, because the i5 has no manager. So the M3 measure covers the Mac's wsp only. Under T1 (d1 Q6), the tree's host is the Mac for every job.

#### M3 group 3: the manager serves jobs (wsp-050, 2026-11-17)

`crates/manager/src/jobs.rs` holds the job table, beside seats.rs. It runs each job on `wsp-host`'s runner
(`crates/host/src/jobs.rs`, wsp-049). serve.rs no longer refuses `job.*`.
- **`job.run` is keyed by `JobKey`.** A retry of a key gets the same journalled `job.started` with the same id. Once
  the job has ended, a retry gets its `job.ended`. It never runs a second time. The retry also takes the job over:
  its output and its ownership go to the connection that asked last. A key asked again with another argv, class or
  place is refused `duplicate`. So is a key whose `<key>.run` is on disk but which is no longer in the table.
- **Capacity per class** comes from `--build` and `--check` (2 and 4 by default) and is reported in `hello`'s
  `Capacity`. It is a hard cap and nothing more. A run past it is refused `full` and is never queued. When to ask
  stays wsp's throttle's call (d1 Q11).
- **Where a job runs.** A `dir` is a path under `--tree-root` (the home directory by default) on this host. A path
  that is absent, not a directory, on another host, or outside the root once links are resolved is refused
  `no_tree`. A row's `tree` is also refused `no_tree` until Q6 places trees on hosts. Under T1, wsp names the
  directory instead.
- **Events.** `job.started {pgid}` and `job.ended {exit, timed_out, hung, log, cpu_ms}` are journalled before they
  are sent, like every manager event. Subscribers get them, and so do the job's owner and anyone who asked it to
  stop. **`job.output` is not journalled.** It goes live to the connection that owns the job and to no other, as
  chunks of up to 64 KiB, each with its byte offset in the log. A client that was not listening reads the log,
  which `job.ended` names.
- **`job.stop` ends the job's own pgid.** It sends `TERM` to the group, then `KILL` once the grace (5 s) is out. The
  manager signals no group that is not in its job table. `manager.log` names every signal it sends with its group,
  and also the group the runner kills at every end (wsp-453). A stop of an ended job gets its `job.ended`, and a
  stop of an unknown key is refused `no_job`.
- **Ownership.** When a connection closes, the jobs it owns that are not `detach`ed are stopped as `job.stop` stops
  them, and the log says why. A detached job runs on, and its output goes nowhere until its key is asked again.
- **`jobs`** answers `jobs.list` with the table: every job the manager runs or ran since it started, with its key,
  class, pgid, start and state.
- **Restart (2.5).** `<exec_dir>/jobs/<key>.run` holds the `job.run`, written before the job starts. At start
  every one of them is joined to the journal and to the runner's `<key>.job` and `<key>.exit`:
  - an end the journal already holds is kept as it is, so it is never reported twice;
  - an end that only the exit file holds (the job ended while the manager was down) is reported `job.ended` now;
  - a job still running whose pid has its recorded start time is re-adopted and watched to its end. It has no
    owner until its key is asked again, so it is not stopped for the connection the restart closed;
  - a job that is gone and wrote no end is reported `job.ended` with no exit;
  - a `.run` with no runner record (the manager stopped between the two) is forgotten.

  `Manager::stop` leaves jobs running in the same way, for the next start to adopt.

Tests (`tests/jobs.rs`) run a loopback manager with `sh` jobs under a scratch tree root. They check that:
- two `job.run`s under one key run once, and a third connection's retry takes the job over;
- `job.stop` ends the job's group, its background child included, while a process outside the job survives, and
  every kill the log names is on a group in the table;
- a run past a cap of one is refused `full` and not queued;
- a dropped connection stops its undetached job, and the detached one runs on;
- the `wsp-manager` binary, killed with SIGKILL mid-job and restarted, reports each job's end once (one job ended
  while it was down, one was re-adopted), starts nothing again, and resumes events `from` the last id;
- `jobs` lists each job as running or ended.

### 6.3 How this reorders state-core section 7

The manager steps go **first**, before state-core's step 0. They need no store and no server, and they take out the
OS work that would otherwise be carried into the server and then extracted again. Then:
- state-core **step 1** (the server, its read path and compound subscribing) folds manager events into the store's
  mirror, rather than mirroring files that probes wrote.
- state-core **step 2** (the orders machine) keeps its state machine, but its delivery half already exists. Delivery
  and ack come from the manager at M1, so step 2 is the store owning the order record and the `deliver:<seat>` lease.
- state-core **step 3** (seats via executors) shrinks to *the server takes the manager connections over from the
  daemon, and the seat machine owns the record*. The ps/lsof deletions it listed are done at M1 and M3.
- state-core **step 6**'s network transport is already in step 1 (D19) and in M1 for managers. Step 6 keeps auth and
  principals.

The amended table is in docs/state-core.md section 7.

---

## 7. Questions for Ed

1. **The manager's shape.** Option A: one manager per host, supervising one durable compound-sup per seat (2.1,
   recommended, D14's lean)? Or B (no manager, wsp to each compound-sup), C (one supervisor for the whole host) or D
   (the wsp daemon on every host)?
2. **The manager and compound-remote.** Should the manager fork compound-sup itself, with compound-remote keeping only
   `gate` (recommended: one process fewer, and the manager runs in the GUI session already)? Or should it delegate to
   compound-remote `serve` as a separate process?
3. **Repository.** R1: wsp's repository, with crates `manager` and `host-api` (recommended)? R2: compound's,
   growing compound-remote? Or R3: a third repository (2.3)?
4. **Who dials.** wsp dials the managers on day one (recommended). Should managers dial out to the server once it
   exists, for cloud hosts that accept no inbound connections? Or should the server keep dialling?
5. **Day-one identity.** Is one token per manager, plus binding to the tailnet and loopback only, enough until D13 is
   discussed? When does TLS come in: at the first host off the tailnet, or from M1?
6. **Where trees live.** T1 (trees on the store's host, D5 as written, the far copy kept), T2 (trees with the agents,
   with land as a fenced push to a trunk remote; this reverses 2026-11-05's "copied, not pushed git to git" for far
   rows) or T3 (copies everywhere, the trunk on one host) (1.3)? The recommendation is T1 now, with M3's jobs and
   M4's verbs reserved so that T2 changes placement only.
7. **Tree bytes under T1.** Should copy-up and copy-back stay rsync over ssh, or become a `tree.copy` stream in the
   protocol so that ssh is left for install only?
8. **The agent's own `wsp` calls on a far host.** Keep the shim's ssh until the server's network API (recommended),
   or relay them through the manager's connection?
9. **wsp-120 at M0.** Is herdr's removal the first line of the manager work (recommended), as D20 put it in the
   first pass? Or does it wait for state-core's first pass?
10. **Renderers.** The manager reports a seat's sockets so that compound's host can attach a compound-render. Should
    *attaching* stay wholly compound's business (recommended), or should the manager also start renderers for seats
    a person is watching?
11. **The throttle's decision.** Should admission (whether a build may start now) move into the manager with the jobs,
    since the facts and the slots are both there? Or should wsp keep the decision and read the facts from the
    heartbeat? This is D5(b) again, at host scale.
12. **The gate for local seats.** Today only far seats are gated, and "a local seat hears nothing". If the Mac is a
    host like any other, should its seats be gated too when its manager stops hearing wsp? That is safer, but a
    daemon restart would then pause the Mac's agents.
13. **Measures.** Is a day of `wsp calls` before and after each step, a week for incidents, and spawn p50 no worse
    than 9.4 s the right bar?
14. **Scheduling.** By D12 nothing is filed until Ed prompts it. When prompted, should M0–M3 be one list with M4 left
    for Q6, or should each step be its own list?
