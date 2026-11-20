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
| leaked agents: agents found by `ps` and killed | src/leaked_agents.rs:126 | **D** (gone, wsp-057) | Every agent on a host is one of the manager's seats. An agent that is nobody's cannot exist, except one started by hand, and that is not wsp's to kill. |
| `wsp role reap`: `ps`, `lsof` and kill | src/cmd_role.rs:360, :629, :671 | **D**, or a manager verb if Ed wants a person's reap. Gone in wsp-057: `wsp reap <pid>` is removed and `wsp reap --list` reads the manager's seats and jobs (wsp-042's (b)) | The same reason. |
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
| I2: is a seat alive, and in what state | `util::alive` per seat, `seat-since.json`, memory.rs's `ps`, waiting.rs's `ps`, herdr's `agent.list` | `turn`, `seat.stopped`, `seats` (`seat-since.json` done: group 7) |
| I3: was an order delivered and taken | screen renders (59.5 per tick), `typed-unsent.json`, `first-orders.json`, the `hooks.jsonl` scans, `stuck-commands.json` | `order.*`, `seat.stuck` (`typed-unsent.json`, `first-orders.json` done: group 7) |
| I4: has a seat ended | census-by-absence, `finished-wip.json` | `seat.stopped` (done: group 7) |
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
| **M3. Tree jobs under the manager** | `throttle run`, `check`, `verify`'s checks and barrier runs become `job.run` on the tree's host (the Mac). The throttle admits by the manager's facts. check_tree's sweep, the slot probes and leaked_agents go. | **lsof** → 0 and **ps** → 0 on the wsp side. Every `kill` is by a manager, on a pgid it started. wsp-478-kind incidents → 0 over a week. `throttle run`'s wall per build no worse. | Jobs fall back to wsp running them in-process behind a flag (`WSP_JOBS=inproc`; the default is `manager` since wsp-056). The sweep is restored with them. |
| **M4. Trees placed (T2, Ed 2026-11-18; see "M3 under T2")** | `tree.checkout` and `tree.land` on every host's manager, a fenced push to the trunk, per-host warm targets, and the barrier's host named. | A list runs with members on two hosts and no copy-up or copy-back. Land conflicts are refused by the push and never overwritten. Cold-build cost on the i5 is measured against remote-agents' early proof. | Rows placed on the Mac only (T1), which is the state at M3. |

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

#### Group 7: the contract (wsp-030, 2026-11-20)

The last code row of M1: with `manager` the default, wsp no longer does what M1's events replaced.
- **Deleted:** `seat-since.json` sampling, `typed-unsent.json`, `first-orders.json`, `repair:unsent`,
  `sweep:ended-panes` and census-by-absence for the Mac's seats (`finished-wip.json`), `Place::turn_began_since`,
  `Host::turn_began` and wsp's own scan of `hooks.jsonl` for a turn. `wake` reads the folded turn
  (`fold::turn_began_since`); `under_manager()` is gone because it is always true.
- **`WSP_HOST=inproc`** now runs the manager's own code in the daemon's process over the same `Tcp`, not the M0
  path. It is the manager without a second process, not a rollback to the old inferences.
- **Left, by plan:** `tunnels` and far.rs's two ssh lines (M2); `leaked_agents`, check_tree's sweep and warm.rs (M3).
  `wait_ready`'s 150 ms `state` loop and `submit`'s poll stay as the fallback for far seats, which have no events
  until M2; `hooks.jsonl` is still written by `log_hook`, and `did.rs` and the `ran` clause still read the
  transcript, since no `seat.*` event replaces them. `docs/host-seam-allowed.txt` holds its four lines.

#### M1's result (wsp-031, 2026-11-20)

**The window.** These verdicts are taken from one window: **2026-11-20T07:58:22Z to 08:41:38Z, 43 min (0.72 h),
128 ticks**. It starts when the M1 trunk (`38e6aa8`, wsp-030) was installed. The daemon's start on that binary is
`daemon.json`'s `since`, 07:58:22Z, and the manager restarted at 07:58:24Z. The person passed barrier 8 at
08:25Z on judgement, without waiting for the M1 day. So `wsp calls --since 24h` still holds mostly ticks from
before M1 (ff9afa6), and is not M1's result. Every figure below comes from `measure.py` in the row's scratch
directory, run over the window. It reads `calls.jsonl` and `polls.jsonl` the way `wsp calls --since` and
`--polls` do. The 24 h before the install (2026-11-19T07:58:22Z to 2026-11-20T07:58:22Z, 4,280 ticks) is
given beside it as the before figure only.

A figure that is a rate per tick or per call is judged on the window when its miss or its hold is far wider
than the window's noise. A figure that is per day, or that rests on a handful of calls, is **owed**: the
day-long figure on the M1 trunk is filed as wsp-323, after wsp-324.

**The installed manager is not the M1 trunk's.** `wsp install` at 07:58Z rebuilt `wsp`. It copied the
`wsp-manager` it found beside it, and that was `target/release/wsp-manager` from 2026-11-19T07:29Z
(`put_manager`, `src/cmd_install.rs`: a binary beside `wsp` is not rebuilt). `cmp` shows the binaries are the
same. The live manager lacks every manager and host commit since then: 3488325, f44f2cb, and wsp-030's
7498607, 9ffd75e and 981dca4. So the window measures the trunk's `wsp` against a manager a day older. The
owed day is to be taken on a manager built from the trunk.

| measure | 6.1 baseline | M1 asks | **M1 trunk, from 07:58:22Z, 0.72 h** | before: 24 h to the install (ff9afa6) | verdict |
|---|---|---|---|---|---|
| `ps` per tick | 37.5 | ≤ 1 | 9.00 (128 ticks) | 6.85 | **missed** |
| renders per tick (`compound-reader`) | 59.5 | 0 | 35.32 | 14.40 | **missed** |
| `df` per tick | 24.6 | 0 | 2.69 | 1.36 | **missed** |
| `lsof` in `wip` | (in the 2.9 s) | 0 | 0 in 54 calls | 0 | met in the window |
| `wip` CPU per call (polls, p50) | 2.9 CPU-s | < 0.2 | 0.671 (n = 23), `--json` 0.647 (n = 31) | 0.428, `--json` 0.483 | **missed** |
| compound's host polling, CPU-s per day | 25,268 | < 2,500 | partial: 20.1 CPU-s in 0.72 h, ~668 a day scaled | 660 | **owed** (full day) |
| spawn p50 | 9.4 s | ≤ 9.4 s | partial: 2 spawns, 1.50 s | 0.93 s (147) | **owed** (full day) |
| tick p50 / p95 | 19.5 s / 116 s | — | 76 ms / 976 ms | 20 ms / 686 ms | — |
| wsp-411-kind incidents | 0 of 30 (M0's week) | 0 over a week | 0 of 5 seats ended | 0 of 79 (25 h) | **owed**, 2026-11-27 |
| manager `kill -9` mid-turn | — | 10 scripted runs | 10 of 10 hold (on the trunk's build) | — | met |

In this window the trunk does worse than the day before it: more renders, more `ps`, and dearer `wip` calls,
on a machine busy with builds. 43 minutes cannot say whether that is load, the stale manager, or the trunk. The
owed day will.

How each was taken, and the cause of each miss:
- **Per tick** is the children counted on every `"k":"tick"` line in the window, over all of them. It includes
  the light ticks that run only `standing` and `attention`. The causes come from the same lines' `phase_kids`
  in the window:
  - **`ps`.** It comes from `ending:verification` (4.03 per tick), `attention` (1.16), `repair:refusing` (0.77),
    `repair:frozen` (0.59), `tunnels` (0.55) and `sweep:build-output` (0.37). These are `Host::alive`,
    `tree_of`, `table` and `rows_of`. `host::Tcp` leaves all of them in-process under both choices, because
    group 4 kept the process table where it was until M3 (`crates/wire/src/host_tcp.rs`, "the process table
    stays in-process at M1"). M1's line asked for ≤ 1 without a row that moves them. This is a gap in the
    breakdown, not a regression.
  - **Renders.** They come from `ending:verification` (9.61), `tunnels` (6.16), `attention` (6.10),
    `sweep:build-output` (4.13), `ending:checks` (3.80) and `sweep:check-trees` (3.80). They are
    `Host::screen_state`, which `Tcp` hands to the in-process host (`host_tcp.rs`, `screen_state`).
    `Compound::detected_state` (`place_compound.rs`) reads the screen first for every seat whose state it is
    asked. The manager's `seats` reply has the hook-folded state (wsp-038), and nothing in the daemon reads it
    in place of the screen. The breakdown never planned a row to make that switch.
  - **`df`.** All of it is `sweep:scratch` (`scratch::tidy`). It asks the disk itself and does not read the
    beat's `HostFacts.disk` (the throttle reads those, wsp-028).
- **`wip`** is user+sys of the process and its waited children, from `polls.jsonl`. It has no children: no
  `ps`, no `lsof`, no render. The 0.67 CPU-s in the window, and the 0.43 the day before, is wsp's own work in
  the process. On the same store, `ls --json` takes 0.04 s. M0's A/B measured 0.131 on the Mac, with 2 seats
  and a smaller store. Here there are 12 seats and a 44 MB store of 4,770 files. This machine has no profiler
  (`perf`, `strace`), so the cost was not isolated to a phase. The cause is named as not found (wsp-327).
- **compound's host polling.** In 6.1 this was `none via host`, the gpui app. Here compound polls over ssh:
  `stamp`, `wip --json`, `ls --json`, `spend --json` and `worklist … --json` under `sshd-session`. In the window
  that is 104 polls and 20.1 CPU-s, about 668 a day if the rate held. Every no-seat poll together, the person's
  shell's `wip` loop (`zsh`) included, is 42.0 CPU-s, about 1,398 a day. The day before, compound's set was
  5,711 polls and 658 CPU-s, and everything was 2,448. The figure is partial, so it is owed, not met.
- **Spawn** is `"verb":"spawn"` with exit 0 in `calls.jsonl`, live, with the agent, as M0's live column. The
  window holds 2 spawns, too few for a p50. The day before held 147 (0.93 s, p95 2.71 s).
- **Incidents** are `wsp calls --incidents`. Since the install: none of any kind, and 0 of 5 seats ended on a
  census were heard from after. On this machine `cycle.log` starts at 2026-11-19T07:36Z, so the week on the M1
  trunk is owed on 2026-11-27. Over the 25 h before: I1 5 (wsp-478 kind 1, barrier check trees 4), I2 3, I3 3,
  I4 1. wsp-411 kind: 0 of 79.

**The `kill -9` proof.** The proof is `k9/proof.sh` in the row's scratch directory, run 10 times by
`k9/ten.sh`. Each run's log, journal, manager log and agent log are in `k9/runs/<n>/`. Each run starts and
ends its own instance:
- a `wsp sandbox --fake` store and state, with a one-commit repo, a project and a row;
- a private `wsp-manager` from this tree's build, on a free loopback port, with its own exec dir. The
  sandbox's manager record names it;
- the sandbox's own `wsp daemon`;
- `wsp spawn <row> --agent`. This gives a real compound-sup session with compound-reader reading its screen.
  The agent is `k9/claude`, a stand-in with no model: it draws Claude Code's composer (`❯` between two
  rules) and the title spinner while a turn runs, and it fires the real hooks (`claude-code/wsp-session.sh`,
  so `wsp report` to the manager and `compound-sup submitted`). Every prompt it is typed is logged.

Order A is told. It is typed, and the seat is mid-turn: the screen is working, and A's `UserPromptSubmit` is
held 2 s. Order B is told behind it. The manager is killed with SIGKILL 0.5 s later, with **both orders in
flight in its `orders/`**. Both `wsp tell`s answer "unreachable — held". The manager is started again 2 s
later, on the same port and exec dir. A run holds when every one of these is true:
- the agent's process and its compound-sup are the same throughout;
- the restarted manager adopts the seat under its epoch, and the journal holds one `seat.started` and no
  `seat.stopped`;
- `wip` lists the seat;
- the brief, A and B were each typed exactly once, and each turn ended;
- the journal acks each order once, and the manager's `orders/` is empty;
- the row is still claimed by the seat, and `cycle.log` ends nothing.

The restarted manager asks both orders again under their ids. compound-sup answers A from its kept answer
without typing it again, and types B once the composer is idle. Runs 1–10, 2026-11-20T08:36Z to 08:39Z: **10 of 10 hold**. Every run passed all 15 checks, had 2
orders in flight at the kill, and re-asked 2 orders on restart (`k9/ten.log`). A run takes 14 s from the kill
to the checks, and most of that is A's 8 s turn.

The first attempt found a hazard, not a failure. A seat's environment carries `COMPOUND_RUN_DIR`, which names
the live manager's run directory, and `wsp sandbox` keeps it. So a manager started in a sandbox from a seat
adopts the live fleet's seats as "started in-process". The private manager adopted all 12, and its first seat
id, `cpd-1`, collided with the live m3 custodian's. It was killed before it acted, and every live seat was
checked alive. The script now unsets the variable, points `--inproc-run` at an empty directory, and stops if
anything is adopted. `wsp sandbox rm` also leaves a manager-forked seat running, because it reaps by
`WSP_STATE` in a process's arguments. The script ends whatever names its sandbox.

**Against the done-when line** (window 2026-11-20T07:58:22Z, 0.72 h, on the M1 trunk's `wsp` and a stale
manager):
- **ps ≤ 1, renders 0, `df` 0: missed.** 9.00, 35.32 and 2.69 per tick over 128 ticks, down from 37.5, 59.5 and
  24.6. The cause is calls that M1's breakdown left in-process (the process table, `screen_state` and the
  scratch sweep's disk), not new ones (wsp-326).
- **`lsof` in `wip` 0: met in the window**, over 54 calls.
- **`wip` < 0.2 CPU-s: missed.** 0.67 in the window and 0.43 the day before, from 2.9. The cause is in-process
  work, not isolated (wsp-327).
- **Host polling < 2,500 CPU-s per day: owed.** It is partial: about 668 a day for compound, and 1,398 for
  everything that polls, scaled from 0.72 h.
- **Spawn p50 ≤ 9.4 s: owed.** 2 spawns in the window.
- **wsp-411 kind 0 over a week: owed** on 2026-11-27. 0 of 5 since the install.
- **kill -9 mid-turn loses no seat and no order: met,** 10 of 10, with two orders in flight at the manager each
  time. The agent is a stand-in, so what this proves is wsp, the manager, compound-sup and the hooks, not
  Claude Code.

Filed from here:
- wsp-323: the figures owed. That is a full 24 h on the trunk's `wsp` *and* manager, no earlier than 24 h after
  the reinstall wsp-324 makes possible (and no earlier than 2026-11-21T07:58Z), and the week of incidents on
  2026-11-27. It comes after wsp-324.
- wsp-324: `wsp install` keeps a stale `wsp-manager`.
- wsp-325: `wsp sandbox` keeps `COMPOUND_RUN_DIR`, and `sandbox rm` leaves manager-forked seats.
- wsp-326: the per-tick `ps`, renders and `df` M1's breakdown left in-process. Which go to M3 is to be decided.
- wsp-327: where `wip`'s 0.43 CPU-s goes.

#### M3's breakdown (wsp-042, 2026-11-17)

The list is `m3-agent-manager`, at status init. It is planned on top of M1 alone, because no far machine is registered and M2 is not available. It follows wsp-018's three rules:
- expand, then contract. A job runner is added beside today's in-process runs, behind `WSP_JOBS=inproc|manager`, which defaulted to `inproc` until group 6 made it `manager` (wsp-056). Callers move one area at a time. The old spawns and the sweeps are deleted last;
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
| 6 | contract | wsp-056 | `WSP_JOBS` defaults to `manager`, and `wsp install` writes the job capacities. Stop: a live day on the installed trunk. Done: see "M3 group 6". |
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
- **Pruning, by age (wsp-062).** The retention is an age, not a count: `Config::job_retention`, a day by default.
  An age bounds what a retry needs, because a retry of a key comes soon after it was asked, while a count would let
  a burst of jobs push out the one a caller is about to retry. The age counts from the job's end. A job is pruned
  only once its `job.ended` is in the journal (the frame has an id), because the journal is the record that the
  end was said and acknowledged. A `job.ended` the journal could not take keeps the job until a restart journals it.
  Until the retention is past, the job keeps its table entry and its files, so a retry of the key gets the same
  `job.ended` with the same id, and the log it names can still be read. Once the retention is past, the job's
  entry is dropped and its files are removed in this order: `<key>.log`, `<key>.exit`, `<key>.job`, and `<key>.run`
  last. If the manager stops part-way, the next start sees the `.run` again. Either the journal's end keeps it
  until the next prune, or it has no record and is forgotten. The beat sweeps every period, and `job.run`, `jobs`
  and a start sweep too. `manager.log` names each job pruned. A key asked again after its job was pruned starts a
  new run, because nothing of the old one is left to refuse it by. So the retention has to be longer than any
  caller's retry window. The journal is never compacted and still holds the old run's `job.ended`. So at start
  each key is joined to the end that follows its latest `job.started`, and an earlier run's end counts for
  nothing. Without this rule, a rerun the restart found running would be taken as ended: it would not be watched,
  and a retry would be answered the old exit.
  That rule alone leaves a window (wsp-064). The runner writes `<key>.job` before the manager can journal
  `job.started`, because the pgid is known only once the job is spawned. A manager killed between the two leaves a
  rerun whose last frame for its key is the old run's `job.ended`. wsp-064 offered two fixes and takes the second:
  at start, a run on disk counts as newer than any end journalled before it. The journal's order stands in for the
  end's time, because frames carry no time. Before the runner starts, the manager writes the journal's head into the
  `.run` (as `after_event`, beside the `JobRun`). At start it counts only the key's `job.started` and `job.ended`
  frames past that id. So the rerun is re-adopted running, and its `job.started` is journalled at that start. The
  first fix, journalling `job.started` before `.job`, would mean splitting the runner's start in `wsp-host`. A
  `.run` written before wsp-064 has no head and is joined as before.

Tests (`tests/jobs.rs`) run a loopback manager with `sh` jobs under a scratch tree root. They check that:
- two `job.run`s under one key run once, and a third connection's retry takes the job over;
- `job.stop` ends the job's group, its background child included, while a process outside the job survives, and
  every kill the log names is on a group in the table;
- a run past a cap of one is refused `full` and not queued;
- a dropped connection stops its undetached job, and the detached one runs on;
- the `wsp-manager` binary, killed with SIGKILL mid-job and restarted, reports each job's end once (one job ended
  while it was down, one was re-adopted), starts nothing again, and resumes events `from` the last id;
- `jobs` lists each job as running or ended;
- an ended job is answered the same `job.ended` on a retry inside a 3 s retention, its log still readable. With no
  client connected, the beat then removes its four files and its row (wsp-062);
- a key run again after its prune, with the manager stopped and started while the rerun is running, is re-adopted
  as running. Its retry gets the new `job.started`, and its own end is reported with a new id (wsp-062).
- a rerun whose `job.started` never reached the journal is re-adopted as running at the next start, not paired with
  the old run's `job.ended`, which is the key's last frame. Its retry is answered `job.started`, and its own end is
  reported after the old one (wsp-064).

#### M3 group 4: `Host::run_job` (wsp-051, 2026-11-17)

The `Host` trait (crates/wire/src/host.rs) gains `run_job`, `stop_job` and `jobs`. They take and give host-api's own
`JobRun`, `JobEnded`, `JobOutput` and `JobRow`, and fail with a `JobError`: silent, refused in the manager's words
(`full`, `duplicate`, `no_tree`, `no_job`), or unanswered. Nothing calls them yet. The callers move in group 5.
- **`host::Tcp`** runs each job on its own connection. It subscribes from the journal's head and then sends `job.run`,
  so that connection owns the job. `job.output` goes to the caller's sink, and `job.started` and `job.ended` are
  heard as they happen, never by polling `jobs` (wsp-037). If the connection drops, it dials again for up to
  `BEAT_SECS × MISSED_BEATS`, subscribes from the last id it heard, and sends the same frame with its key unchanged.
  The manager hands the job over and does not run it again. `stop_job` and `jobs` go over the CLI's one connection.
- **`host::InProcess`** runs wsp-host's runner here (`with_jobs(exec, tree_root)`; by default
  `~/.local/state/wsp/inproc` and the home directory; `WSP_JOBS=inproc` gives it `<state>/inproc` and the tree root
  in `<state>/manager/jobs.json`, see "One root on both sides" below). It places a `dir` by the manager's rule, uses the same 5 s
  grace between `TERM` and `KILL`, and keeps a job table per process. A key the runner's files show already ran
  gets back that run's end and is not run again.
- **`WSP_JOBS=inproc|manager`** is read once per process (`host::jobs_chosen`). The default was `inproc` until wsp-056 made it `manager`. `manager`
  is the same `Tcp` that `WSP_HOST` uses, but the two variables are independent. A silent manager is "host silent",
  and the job is not run in-process instead.
- **Keys** come from `host::job_key(row, attempt, job)`, built once per run by the caller. `job` must be unique per
  run (wsp-040).

Tests (crates/wire/tests/host.rs) hold both hosts to one job contract on a loopback manager. A job's exit and output
come back. One key sent twice runs once, and the next attempt runs again. A stop ends the group and its background
child. A timeout says `timed_out`, and a `dir` outside the root is refused `no_tree`. They also check that a silent
manager is "host silent" within a second with nothing run, that `WSP_JOBS` is chosen independently of `WSP_HOST`,
and that a job whose manager restarts mid-run is resent under its key and runs once.

#### M3 group 5: `throttle run` under `WSP_JOBS=manager` (wsp-052, 2026-11-20)

`wsp throttle run` (crates/trees/src/throttle.rs) is the first caller to move. Under `inproc`, and on a host with no
manager record (the i5 before M2), nothing changes: `run_in` runs the build in this process as before. Under
`manager`, `run_raw` takes `run_raw_on`, and `run_on` keeps the decision here (d1 Q11):
- **The decision reads the beat.** The person's idle time, the clock and the cores come from `Host::facts`, which is
  the facts on the manager's `seats` reply (hello carries none), and not from `ioreg`, `pmset` or `sysctl`. Through
  the same reading (`View::of_host`, `Given::of_facts`), `wsp throttle`, `wip`'s files-path line, `doctor` and the
  daemon's `sweep:throttle` see the same machine. With no beat yet there is no load and no clock, and `auto` holds.
- **An admitted run is one `job.run`.** That is a heavy build or a `cargo run`, at the shim's cwd as a
  `HostDir {host: the manager's own name (Host::job_host), path: relative to its tree root}`. The tree root is the
  home directory, which is the manager's default. Class `check` is for `cargo check` and `clippy`, and `build` is
  for the rest. The argv is `nice -n 12` when niced, then `env -i` with the allowlist (`PATH`, `HOME`, `TERM`,
  `CARGO_TERM_COLOR`, `CARGO_*`, `RUST*`, `SCCACHE_*`, `WSP_*` with no `TOKEN`, `KEY`, `SECRET`, `PASS`, `AUTH`,
  `CRED` or `COOKIE` in the name), then the hold and the job cap the decision set, then the real tool. The output
  goes to the shim's stdout as `job.output` arrives (the job's log is one stream, so stderr is in it). The exit is
  the job's. A tool's own signal is the shell's 128+n. A job ended by a stop or its bound exits 143, and one the
  shim's ^C stopped exits 130. The job is not detached, so a shim killed with its agent stops it. A run refused
  `full` waits and asks again under the same key. Any other refusal, and a silent manager, run nothing here
  instead.
- **^C is `job.stop`.** The job is in its own process group, not the terminal's, so the shim catches SIGINT
  while the job runs and stops it.
- **Holds carry the job's key and are read from the job table** (`live_by`). A running job holds. An ended job does
  not, with no `kill -0`. A job not in the table yet holds for 60 s from the hold's mtime, and the holder refreshes
  it while it waits for a slot or for `full`. A hold that is no job's (a check's slot, an in-process run), or one
  whose table could not be read, is judged by its pid as before. wsp-053 moves the check slots.

Tests (crates/trees/src/throttle.rs, `manager_tests`) run a loopback manager with `sh` standing in for cargo. One
run is one row in `jobs`, and its output, exit, cap, hold and nice reach the caller with no token in the
environment. A slot left in this process's own name, whose job has ended, is taken at once with no pid asked. A
beat that says the person left, the person is here, or the clock is limited drives the decision. `run_raw` run as
its own process ships the build as its job, and its call log has none of `sysctl`, `pmset`, `ioreg`, `vm_stat`, `ps`
or `kill`. SIGINT to that process stops the job (exit 130, the tool gone, the job ended in the table).

#### M3 group 6: `WSP_JOBS` defaults to `manager` (wsp-056, 2026-11-20)

- **The default.** An unset or empty `WSP_JOBS` is `manager` (`host::JobsChoice`, in `crates/wire/src/host_tcp.rs`,
  re-exported by `host.rs`). `WSP_JOBS=inproc` still runs jobs in this process through wsp-host's runner, and is the
  rollback. A silent manager is still "host silent", and an unset `WSP_JOBS` never falls back to in-process.
- **The capacities.** `wsp install` writes `--build`, `--check` and `--tree-root` into the manager's launcher (the
  plist on macOS, the systemd unit's `ExecStart` on Linux), after `--port`. It takes them from `manager::Jobs`.
  `--build` is the throttle's slot count on this machine's cores (`throttle::cores`, a pinned count first), and
  `--check` is its check slots. Both use the cool counts, the larger, so the manager's hard cap never refuses what
  the throttle admits. A hand's `slots` and `checks` in `throttle.json` carry through. A change to either is a
  rewrite of the launcher and a restart.
- **The tree root.** Each host's install writes its own manager's tree root, as T2 asks. If every registered
  project root is under the home directory, the root is the home directory (the manager's default). Otherwise it
  is the deepest directory all the project roots share, for example `/Volumes/Work/src`. A project root is also
  where its trees are made at M4 (`.worktrees` under it), so the root covers them too. If the roots share nothing
  below `/`, the install says so in red, since then no job is confined. `--tree-root` stays single: a `HostDir` is
  a path relative to one root, so a repeatable root would change the contract. Today's project roots on each host
  share one volume.
- **One root on both sides.** The install keeps what it told the manager in `<state>/manager/jobs.json`, beside the
  machine record, and `--uninstall-manager` removes it with the record. wsp places a job under the same root:
  `job_place::tree_root` and `throttle run`'s `Jobs` read it there (`job_place::installed_tree_root`). Without the
  file, wsp uses the home directory, as before. The rollback's in-process runner (`WSP_JOBS=inproc`,
  `host::choose_jobs`) reads the same file by the same rule, so a host whose projects are outside home still runs
  its jobs in their trees after a rollback.
- **Doctor** names the capacities in the manager's `hello` ("runs N build and M check jobs at once"). It reports a
  problem when they differ from what `jobs.json` says the install wrote.

Tests: `crates/wire/tests/host.rs` runs a job with `WSP_JOBS` unset on a loopback manager. The manager's host and
its exec dir keep the log. The same job under `inproc` runs in this process, with its log under `<state>/inproc`.
`launchd.rs` and `systemd.rs` check that the launcher carries the capacities in order. `manager.rs` checks that
doctor names `hello`'s capacities and holds them to `jobs.json`. `cmd_install.rs` checks that the launcher `wsp
install -n` compares against carries the throttle's settings and a root covering the projects.
`job_place.rs` checks that wsp reads back the root the install wrote.

#### M3 group 7: the contract (wsp-057, 2026-11-20)

wsp-057 deleted what the jobs replaced. Gone: check_tree's process sweep, `leaked_agents` and `wsp reap <pid>`; the
throttle's slot probes and check slots; sharing's `alive`, the barrier run's pid start time (`checks::Run`'s
`started`, `lstart`) and the `TreeLock`'s `kill -0`; and every direct spawn of tree work: check's steps, verify's
cargo, the throttle's runs, warm's removal and the `at_stop:` teardown are each a job through `Host::run_job` under
both `WSP_JOBS` choices. With them went the Host methods nothing called (`cwds`, `parents`, the trait's `started`).
A holder's liveness is now its job in a job table or a lease it beats (`wsp_store::lease`: a tree lock, a barrier
run), and no process is probed. `tests/host_seam.rs` lost its tree-work exception: a spawn of `sh`, `cargo` or
`rustc` in tree work's code, or a place built by hand, fails it. Kept under T2 (wsp-093): `place_of` and the rest of
`job_place`, where M4's `tree.checkout` goes, and treecopy.rs, which M4 retires once no host takes copies.

#### M3's result (wsp-058, 2026-11-20)

**The window.** These verdicts are taken from one window: **2026-11-20T12:08:12Z to 12:16:30Z, 0.14 h, 24
ticks**. It starts at `wsp install` of the M3 trunk (`afa711e`, wsp-057). The manager restarted at 12:08:12Z,
and the daemon runs the installed binary (the same inode). This row was started minutes after the install,
not a day after it. So every figure that rests on a day or a week is **owed**, and is filed as wsp-360. Every
figure comes from scripts in the row's scratch directory:
- `measure.py`, which reads `calls.jsonl` and `polls.jsonl` the way `wsp calls --since` and `--polls` do;
- `pgids.py`, which reads the manager's `manager.log` and `journal.jsonl`;
- `rollback.sh`.

Their output is kept beside them (`window.txt`, `before.txt`, `before-pgids.txt`, `rollback/`). The before
figure is the 24 h to the install (2026-11-19T12:08:12Z to 2026-11-20T12:08:12Z, 4,268 ticks). That day ran
three binaries in turn: `ff9afa6`, then the M1 trunk from 07:58Z, then a later trunk from 10:51Z.

**Under T2 (wsp-093): the measure is one host's.** It was taken on the host the fleet runs on, `omarchy`
(Linux, under systemd). That host is the trunk's host and has the only manager, which is the role 6.2 gives
the Mac. Figures per host are left to M4's measure.

**The installed manager is not the trunk's, again.** The install copied the `wsp-manager` it found beside
`wsp`: `target/release/wsp-manager` from 2026-11-19T07:29Z (sha256 `7de0ac04…`). That is the stale build M1's
result named, because wsp-324 is still open. Its job runner is wsp-050's (2026-11-17), so the window does
measure jobs under a manager. But it lacks afa711e's and wsp-030's host commits. The owed day is to be taken
on a manager built from the trunk, so wsp-360 comes after wsp-324. The rollback proof below used a manager
built from this tree.

| measure | 6.1 baseline | M3 asks | **M3 trunk, from 12:08:12Z, 0.14 h** | before: 24 h to the install | verdict |
|---|---|---|---|---|---|
| `lsof` per tick | 0.9 | 0 | 0 (24 ticks) | 0.34 | met in the window |
| `lsof` in CLI calls / `throttle run` / polls | — | 0 | 0 / 0 / 0 (238, 8, 90 runs) | 44 / 0 / 0 | met in the window |
| `ps` per tick | 37.5 | 0 | 9.42 | 7.82 | **missed** |
| `ps` in CLI calls / `throttle run` / polls | — | 0 | 22 / 0 / 6 | 4,678 / 0 / 2,035 | **missed** |
| `kill` per tick | 1.3 | 0 | 0 | 0.41 | met in the window |
| `kill` in CLI calls / `throttle run` / polls | — | 0 | 0 / 0 / 0 | 3,057 / 725 / 686 | met in the window |
| signals in manager.log on a pgid the manager started | — | every one | 6 of 6 | 354 of 354 (from 2026-11-19T07:29Z) | met in the window |
| processes that escape a seat's group, counted (wsp-042 (c)) | — | reported and counted | no reporter | — | **missed**, no reporter until wsp-359 |
| wsp-478-kind incidents | 0 (M0's week) | 0 over a week | 0 | 2 (in 29 h of cycle.log) | **owed**, 2026-11-27; 0 by construction |
| `throttle run` wall per build (exit 0, ≥ 10 s), p50 | — | no worse | 1 run, 28.1 s | 24.2 s (338 runs), p95 88.4 s | **owed** (full day) |
| spawn p50 | 9.4 s | ≤ 9.4 s | 1 spawn, 0.89 s | 1.02 s (213), p95 3.12 s | **owed** (full day) |
| tick p50 / p95 | 19.5 s / 116 s | — | 80 ms / 807 ms | 23 ms / 954 ms | — |
| `WSP_JOBS=inproc` runs a `wsp check` in-process | — | shown once | the step: holds (`rollback.sh`); the step's cargo: goes to the manager | — | **partly met** |

How each was taken, and the cause of each miss:
- **`lsof`, `ps` and `kill`** are the children recorded on each record's `children`. These are counted for
  the daemon's `"k":"tick"` lines, the CLI's `"k":"call"` lines (with `throttle run` counted apart), and
  `polls.jsonl`. They are given per tick, per phase and per verb. wsp signals a group through `signal_group`,
  which runs `kill` as a child, so every signal wsp sends to a group is counted here. The call log cannot see
  two things. One is std's `Child::kill` on a child whose handle wsp holds: the install, `packet_gen`, and the
  ssh of tunnel.rs and far.rs. That sends SIGKILL by syscall to wsp's own child. The other is `pid_runs`'s
  kill(2) with signal 0, which sends nothing. Neither reaches a pgid that wsp did not start.
  - **`lsof` and `kill` went with wsp-057.** The day before, `kill` came from the throttle's slot probes
    (`kill -0`): 2,295 in `throttle`, 725 in `throttle run`, 686 in polled `wip`s, and `sweep:throttle`'s 0.41
    per tick. It also came from `check`'s steps (626). `lsof` came from `ending:verification` and `despawn`.
    None of these runs any in the window.
  - **`ps` is missed.** It comes from these phases, per tick: `ending:verification` 2.67, `attention` 1.29,
    `cycle` 1.08, `repair:refusing` 1.00, `tunnels` 0.96, `repair:frozen` 0.67, `closing` 0.42,
    `ending:checks` 0.33 (barrier check seats), `seats` 0.33, and `sweep:build-output` 0.33 (whether a pane
    stands in a tree). In the CLI it comes from `brief`, `spawn`, `despawn`, `tell` and `worklist`, and from
    polled `ls` and `worklist show`. `sandbox`'s 6 are `wsp sandbox`'s own reap, run by this row's rollback
    proof. All of these are seat liveness and the fleet's census. wsp-326's decision d1 gave those to an M1
    follow-up, wsp-330 (todo), and not to M3. That none of them is a probe from tree work rests on
    `tests/host_seam.rs` (wsp-057), which fails on such a probe, not on a trace. The phases that were tree
    work the day before (`sweep:check-trees` 0.37 and `sweep:leaked-agents` 0.33 per tick) run none: the
    first runs no `ps`, and the second is gone. So the line as written, `ps` → 0 on the wsp side, is missed.
    The part of it that M3's breakdown moved holds.
- **Signals** are matched by `pgids.py SINCE [UNTIL]`. It reads every line of `manager.log` that sends a
  signal:
  - `job K: TERM to its group P` and `KILL to its group P` (a stop or a bound);
  - `job K ended: …; its group P killed` (every end kills the group, wsp-453);
  - `seat S stopped (epoch E, …)`.

  A job's line matches when the journal holds a `job.started` with that key and that pgid. A seat's line
  does not name a group. It matches a `seat.started` for that seat and epoch, whose pid (its compound-sup)
  leads the seat's group. A seat that exits on its own logs the same line with no signal sent, so the count
  over-counts signals and never under-counts them. A stopped seat that the manager adopted, but did not
  start, is listed apart. Any other line that names a signal, `kill`, TERM or KILL is printed as
  unclassified. The script exits 1 if anything is unmatched or unclassified. It was checked against a log
  written for the purpose, with a job group the journal lacks, a stopped adopted seat and an unknown SIGKILL
  line. It named all three and exited 1. In the window, 6 of 6 match (5 job ends and 1 seat stop). Over the
  manager's whole log before the install, 354 of 354 match (102 job ends and 252 seat stops). One path signals
  without a line: seats.rs sends KILL to a compound-sup that never gave its handshake, which is a failed
  spawn. The log holds no such failure.
- **The escape count is missed, for want of a reporter.** wsp-042's decision (c) says that once the sweep
  goes, processes that escape a seat's group are reported and counted, and stopped by no one. wsp-057
  deleted the sweep, and with it the only reporter of such processes on wsp's side: `LiveTree::left_in`
  (src/cmd_spawn.rs) now answers nothing. The manager does not count them either: its seat and job
  bookkeeping knows a group, not what left it, and manager.log has no line for an escape. So M3 has no
  number for decision (c) in the window. This is not a 0: an escape in the window would have gone unseen. wsp-359 adds the
  count, and the owed day (wsp-360) reads it once wsp-359 lands.
- **Incidents** are counted by `wsp calls --incidents`. Since the install, I1, I2 and I4 are 0, and I3 is 1
  (a prompt read off the screen). The wsp-478 kind is 0, **by construction**: wsp-057 deleted its writer,
  `ending.rs`'s `stop_finished` (32df421), and with it the sign that the count reads. So the week's count will
  be 0 whatever happens. What stands in for it is `pgids.py` over the same week, with `kill` at 0 in the
  call log. Both are owed on 2026-11-27T12:08Z (wsp-360). In the 29 h before the install, cycle.log holds 2
  such stops: wsp-139 at 2026-11-19T07:49Z and wsp-120 at 2026-11-20T09:01Z.
- **`throttle run`'s wall** is taken from `throttle run` calls that exit 0. The call log does not name
  cargo's subcommand, so a build is read as a run of 10 s or more. The day before gives a p50 of 24.2 s, a p95
  of 88.4 s and a mean of 30.2 s over 338 runs. The part of that day on the in-process path alone, up to the
  10:51Z install, gives a p50 of 24.5 s (299 runs). The window holds one such run (28.1 s), too few to judge.
  Under the manager the build is no longer a child of `throttle run`: it shows no children in the window,
  where every run the day before had `cargo`. So its CPU is the manager's, and this figure is wall time only.
  In the window, 7 of 8 runs did not exit 0:
  - 4 exited 101. They are wsp-128's tests failing, and manager.log has them as jobs that exit 101.
  - 3 exited 126, from cpd-271. **Exit 126 is a build the shim did not run.** It began with group 6's
    default. The call log holds 17 such exits, none before the 10:51Z install. Some are wsp-353's (the
    manager's tree root left at `~`). The rest are filed as wsp-361: a cargo run from outside the tree root is
    refused, with nothing run. The tree root is `/mnt/data/src`, and outside it are a seat's
    `$WSP_SCRATCH` and the home directory. `cargo --version` in this row's scratch directory reproduces it.
    The same root refuses `wsp check <id>` in wsp's shared check tree, which is under the state directory
    (`~/.local/state/wsp/checks/wsp-wsp/tree`). That happened to wsp-057 and wsp-128, whose checks wsp ran
    itself on a ready row, and to this row's own `wsp check --at`. Every check job that did run was in a row's
    own worktree.
- **Spawn** is `"verb":"spawn"` with exit 0, live, with the agent, as in M0's live column. The window holds 1
  spawn (0.89 s).
- **The rollback** is `rollback.sh`, run at 2026-11-20T12:15:23Z. It starts and ends its own instance:
  - a `wsp sandbox --fake` store and state, with a one-commit repo, and a project whose `## Checks` has one
    step. The step prints the command lines of its first eight ancestors;
  - a row;
  - a private `wsp-manager` built from this tree. It is named for this host, since a row's jobs go to the
    manager of `job_place::host_of`. It runs on a free loopback port, with its own exec dir and tree root,
    and the sandbox's manager record and `jobs.json` name it. The seat's `COMPOUND_RUN_DIR` and manager
    variables are unset first (wsp-325).

  `wsp check <row> --at <commit>` was run twice. With `WSP_JOBS` unset, the step was a job of the private
  manager: a `job.started` in its journal, its log in the manager's `jobs/`, and `wsp-manager` the first of
  its ancestors to be either `wsp-manager` or `wsp check`. With `WSP_JOBS=inproc`, the manager's journal gained
  no job, and the step's log is under the sandbox's `<state>/inproc/jobs`. Its first such ancestor is `wsp
  check <row> --at …` itself. Both runs exited 0. The manager was then ended, along with anything naming the
  sandbox, and the sandbox was removed. **It holds**, 6 checks of 6 (`rollback/run.log`, the run at
  12:21:33Z).

  **But the rollback does not reach the cargo a step runs.** check.rs removes every `WSP_*` variable from
  a step's environment, so the cargo shim inside the step reads `WSP_JOBS` as unset, which means `manager`.
  `rollback.sh` prints this as the step's `WSP_JOBS=unset`. Live, `WSP_JOBS=inproc wsp check wsp-058 --at
  58f7f6f` ran its steps in-process, and its `build` step's cargo went to the manager. The manager refused it
  with exit 126, since the shared check tree is outside the root (wsp-361; `check-inproc-wsp-058.txt`). A
  rollback taken because the manager is down would still send every check's build to it. Filed as wsp-362.

**Against the done-when line** (window 2026-11-20T12:08:12Z, 0.14 h, on the M3 trunk's `wsp` and a stale
manager):
- **`lsof` → 0 on the wsp side: met in the window.** It is 0 per tick and in every verb, down from 0.34 per
  tick and 44 in the CLI. The day is owed (wsp-360).
- **`ps` → 0 on the wsp side: missed.** It is 9.42 per tick, with 22 in CLI calls and 6 in polls. The cause is
  seat liveness and the census (wsp-330), which wsp-326's d1 left outside M3. Tree work's part of it is 0.
- **Every `kill` by a manager, on a pgid it started: met in the window.** All 6 signals match a pgid in the
  journal, and the call log holds no `kill` from wsp. The day before it held 4,468 in calls and polls, and
  1,759 in ticks. Over the manager's log
  before the install, 354 of 354 match.
- **Escapes from a seat's group reported and counted (wsp-042 (c)): missed.** Nothing reports them since
  wsp-057 deleted the sweep (`LiveTree::left_in` answers nothing), and the manager keeps no count. The figure
  has no reporter until wsp-359, and wsp-360 reads it after that.
- **wsp-478-kind incidents → 0 over a week: owed** on 2026-11-27T12:08Z (wsp-360). It is 0 since the install,
  but the sign's writer is gone, so the count cannot rise. The week's `pgids.py` is the real figure.
- **`throttle run`'s wall per build no worse: owed.** The window holds one build (28.1 s, against a 24.2 s
  p50). Builds from outside the tree root, and `wsp check` in the shared check tree, are not run at all
  (wsp-361).
- **Spawn p50 ≤ 9.4 s: owed.** There was 1 spawn (0.89 s), against 1.02 s the day before.
- **The rollback works: partly met.** `WSP_JOBS=inproc` ran a `wsp check` in-process on a sandbox row, with a
  manager present and unused, and the script ended the sandbox itself. But a step's own cargo still goes to
  the manager, because the step's environment drops `WSP_JOBS` (wsp-362).

Filed from here:
- wsp-360: the figures owed. That is a full 24 h on the trunk's `wsp` *and* manager, no earlier than 24 h
  after the reinstall wsp-324 makes possible (and no earlier than 2026-11-21T12:08Z), and the week of
  incidents with `pgids.py` beside it, on 2026-11-27T12:08Z. It comes after wsp-324.
- wsp-361 (high): under `WSP_JOBS=manager`, work outside the manager's tree root is refused and not run. That
  covers `wsp check` in the shared check tree, and `throttle run` in a seat's scratch or the home directory.
- wsp-362 (high): `WSP_JOBS=inproc` does not reach a check step's cargo.
- Owed elsewhere: the escape count waits on wsp-359 (filed by wsp-057), and wsp-360 carries it.
- On wsp-324, a note that it bit again at M3.

M4 is broken down by the governor once this row is done (d1 Q14).

#### M3 under T2 (wsp-093, 2026-11-18)

Ed answered Q6 on 2026-11-18 (relayed by Jesper; the full set is on his wsp-652): **trees go straight to T2.**
Trees live with the agents on each host, and a land is a fenced push to a trunk remote. **d1's Q6 ("T1 now",
2026-11-17) is superseded by this answer**, for M3 groups 5-8 and for M4. The source is Ed's answers as relayed by
Jesper and recorded on wsp-093's overview. Jesper confirmed it on 2026-11-18 (below), provisionally, pending Ed's
confirmation. d1's other answers stand, Q14
included. Recording the project decision is his to do. Ed's other answers change nothing here. There
is one manager per host over compound-sup, wsp dials the managers, and wsp keeps the throttle decision (Q11). The
renderer answer needs no row either: the manager starts no renderer, it reads the render socket from compound-sup's
handshake and reports it with the seat's sockets.

Groups 1-4 (wsp-044, -049, -050, -051) already hold under T2. A job's place is host-api's `JobAt`: a `Tree {row,
host}` or a `HostDir {host, path}` relative to that host's tree root. Neither is a path on wsp's machine, and
`tree.checkout` and `tree.land` are reserved verbs. What T2 changes is *which host* a place names, and that trees are
made and landed by that host's manager.

**Which host's manager runs a job.** The tree's host, which is the agent's host. A row's host is chosen once, when its
seat is spawned (`spawn --on`, the Mac by default), and the store keeps it with the row. Every job for the row goes to
that host's manager, and the `host` in its `JobAt` is that host:
- `throttle run` is called by the agent's shell, so it already runs on the tree's host. Its job goes to the manager on
  the host the shim runs on, over loopback, at the shim's cwd relative to that host's tree root. wsp's admission code
  runs in the shim and reads that host's beat. Slots are per host.
- `wsp check` and `verify`'s builds run on the row's host, in trees made there.
- A barrier runs on the list's barrier host. By default that is the trunk's host (remote-agents' rule that "the
  barrier stays here"), and a list may name another.
- wsp picks the connection by the `JobAt`'s host. While the Mac's manager is the only one (until M2), a row on a host
  with no manager keeps today's in-process path there, as wsp-042 said for the i5.

**How check, verify and barrier find the tree.** Through one function, never by joining a path. The callers in group
5 take a job's place from `place_of(row | barrier, kind)`, with kind one of `work`, `check`, `verify` and `barrier`. It
answers a `JobAt` and makes the tree if it is not there. In M3 it makes check, verify and barrier trees in-process
on the Mac, as today, and answers a `HostDir` on the Mac. At M4 its body becomes `tree.checkout` on the place's host,
whose manager makes the tree from that host's clone of the trunk (the row's own branch is already there) and answers
the `HostDir`. The callers do not move twice. A barrier tree is the trunk at the group's tip, fetched from the trunk
remote by the barrier host's manager. Locks and run starts are read from the job table of the manager on the tree's
host, by key, not with `kill -0`.

**Land as a fenced push.** `land` (cmd_checkout.rs:2666) is no local fast-forward under T2. It becomes `tree.land
{row, expect}` on the row's host:
1. `wsp verified --holds` records the trunk head `H` that the verdict was read on. Today it already rebases on the
   trunk and reruns the checks where the trunk moved.
2. The row's host's manager fetches the trunk remote and checks that the row's tip descends from `H`. Then it pushes
   `<tip>:refs/heads/<trunk>` with `--force-with-lease=refs/heads/<trunk>:H`.
3. The remote's ref update is a compare-and-swap, so two hosts cannot both land on `H`. A push that is refused
   because the trunk moved lands nothing. The row is rebased on the new head, its checks are rerun where the trunk
   moved, and it is pushed again under the new lease. Nothing is ever pushed without a lease, and nothing is
   overwritten.
4. The pushing manager records the outcome of every fenced push, accepted or refused, as a land event: the row, the
   old head `H`, the new head, and for an accepted push the commits it carried. The event is journalled as
   `tree.landed` and folded into the store, and that is the landing record. `Landings` (cmd_checkout.rs:1438) reads
   the trunk's reflog today. A bare repository has `core.logAllRefUpdates` off, so the bare trunk keeps no reflog for
   it to read. `placed` therefore reads the store's land events, and reads the reflog only for lands made before T2.

Each host's trunk-tracking ref moves at its next fetch, which the manager runs at every spawn and every land. The
person's checkout becomes a clone that pulls. The trunk must be a bare repository, because git refuses a push to the
branch a non-bare checkout has out.

**What is left for M4.** M4 is no longer "if Q6 says T2". It is `tree.checkout` and `tree.remove` on every host's
manager (the body of `place_of`), `tree.land` as above with the landing record, the trunk remote, the barrier's
sweep of member trees as `tree.remove` sent to each member's host (worklist.rs:1211 `sweep`), every host's manager
sweeping its own trees, warm targets per host, and `wsp checkout` by a person. remote-agents' copy (treecopy.rs) is
retired there, once no host takes copies. M4 needs a second manager to measure (M2), and it is broken down at M3's
end, as d1 Q14 says for every step.

**Groups 5-8 under T2.** Each row's overview carries a "Under T2 (wsp-093)" paragraph:

| group | row | under T2 |
|---|---|---|
| 5 | wsp-052 throttle run | **Kept, with a note.** It is local by construction. The job goes to the shim's own host's manager, admission reads that host's beat, and slots are per host. |
| 5 | wsp-053 check | **Rewritten.** The check tree's place comes from `place_of`, and jobs are routed by the `JobAt`'s host. Lock and start liveness are read from that host's job table. A test pins that no job place is built from a path on wsp's machine. |
| 5 | wsp-054 verify | **Rewritten** as wsp-053. Verify runs on the row's host, and warm-tree locks are per host. Land is not in this row (M4). |
| 5 | wsp-055 barrier | **Rewritten.** The barrier tree comes from `place_of` on the list's barrier host (default: the trunk's host). The barrier's sweep stays as it is until M4. |
| 6 | wsp-056 default | **Kept, with a note.** Every host's install writes its own capacities and tree root. The tree root has to cover the trees the manager will make at M4, not only the project roots of today. |
| 7 | wsp-057 contract | **Kept, with a note.** The `place_of` seam stays. treecopy.rs is not deleted here (M4). |
| 8 | wsp-058 measure | **Kept, with a note.** The measure is the Mac's alone, because the Mac's manager is the only one. |

**Jesper's answers (2026-11-18, about 15:27Z, every one the recommended option): provisional, pending Ed's
confirmation.** He gave them in the m3-t2-plan pane, to a question put to him directly, and the governor relayed them.
They replace the answers the governor seat gave earlier (ask m-dmyftmwv5200-pacf40), which were not the person's.
Ed owns this architecture, so they stand only until he confirms or changes them. If he changes one, 6.2 and the
affected rows among wsp-052..058 are amended to match:
- **d1's Q6 is superseded.** T2 replaces "T1 now".
- **The trunk remote** is a bare repository per project on the Mac, reached over ssh on the tailnet (1.3, "What T2
  costs", item 1). The Mac stays the trunk's host and the barrier's default host, and the person's checkout becomes
  a clone that pulls. The alternative put to the person was the forge.
- **Verify runs on the row's host.** The verifier is spawned where the row's tree is, with no fetch and no copy.
- **The landing record is the store's**, from the push's `tree.landed`. The reflog is read only for lands made
  before T2.
- **M4 is broken down at M3's end**, after wsp-058, as d1 Q14 says. Every answer is the recommended option, so
  wsp-052..058 need no change and go ahead as amended.

**No row in groups 5-8 needs `tree.checkout` or `tree.land` to meet its done-when.** At M3 the Mac is the tree's host
for every row. So `place_of` makes check, verify and barrier trees on the Mac in-process, and every job is named by
`(row, host)`. No row in groups 5-8 lands. Land stays today's local fast-forward until M4's fenced push. So no verb is
pulled into M3.

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
   **Answered 2026-11-18 by Ed: T2.** See 6.2, "M3 under T2".
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
