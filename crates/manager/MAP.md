# wsp-manager

The agent manager (`docs/agent-manager.md`, M1 groups 2 and 3, M3 group 3; wsp-021, wsp-022, wsp-050): one process per host, binary
`wsp-manager`.
It depends on `wsp-host-api`, `wsp-base` and `wsp-host`, and on nothing of `wsp-store` or `wsp-wire` (2.3 R1,
d1 Q3). `tests/layering.rs` (`the_manager_links_no_store_and_no_wire`) walks its path dependencies to keep it so.
It runs seats and their orders (wsp-022), and tree jobs (wsp-050). Nothing in wsp dials it until wsp-025.

## Holds

- `src/lib.rs`: `Config`, `Manager` (`start`, `emit`, `stop`, `wait`), `StartError`, `allowed` (loopback and
  the tailnet's ranges, nothing else: d1 Q5), `tailnet_addrs`, the beat thread, the accept loop, the
  `<exec_dir>` lock, and the start check that dials its own port first (2.4)
- `src/serve.rs`: one client's connection. The `hello` and its token come first: a wrong token is answered
  `refused unauthorised` and then closed, as is a first frame that is not a `hello`. Then come requests, the
  subscription, and the drop after `MISSED_BEATS` silent beats. Two threads per client: one reads and
  answers, the other writes whatever is queued. A connection whose first line is a `hook.post`
  (`wsp_host::hooks`) is a seat's hook instead: its token is checked, it is folded into events that are
  journalled and sent, it is answered `hook.taken {events}` or `hook.refused {why}`, and it is closed (wsp-023)
- `src/seats.rs`: the seat table (wsp-022). `seat.spawn` keyed by `(row, attempt)`, forking one compound-sup
  per seat itself; `seat.stop` with its grace; `order.deliver` and `order.cancel` through `compound-sup deliver
  <id>`; `seats` (vitals per seat, one `ps` for rss and shells, no render); `seat.read` (the one render); seat
  epochs; and `adopt`, which rebuilds the table at start from `<exec_dir>/run` and the journal, and asks an
  order in flight again under its id; and adopts a seat the in-process host started (wsp-029), a labelled
  session in `Config::inproc_runs` (`--inproc-run`, default compound's run dir) whose one compound-sup `ps`
  names, under the next epoch with its `seat.started` journalled
- `src/jobs.rs`: the job table (wsp-050), over `wsp_host::jobs`, wsp-049's runner. `job.run` keyed by
  `JobKey`: a retry gets the same journalled `job.started` or `job.ended`, and takes the job over (its output
  and ownership). A hard cap per class, `Config::build` and `Config::check`. A `dir` placed under
  `Config::tree_root`. One watcher thread per job: it sends `job.output` to the owner, carries out `job.stop`
  (`TERM` to the job's group, `KILL` after `Config::job_grace`) and journals `job.ended`. `jobs`; `closed`,
  which stops a leaving client's undetached jobs; and `adopt`, which joins each `<key>.run` to the journal and
  the runner's files at start
- `src/journal.rs`: `<exec_dir>/journal.jsonl`. One frame per line. Ids run from 1 per manager. A torn last line
  is cut at open. `after(from)` serves `subscribe`
- `src/token.rs`: `<exec_dir>/manager.token`, made at first start (32 random bytes as hex, mode 0600) and
  narrowed back to 0600 if it was ever widened
- `src/log.rs`: `<exec_dir>/log/manager.log`, rotated once past 10 MB
- `src/main.rs`: the flags (`--exec-dir`, default `$WSP_MANAGER_DIR` or `~/.local/state/wsp-manager`;
  `--host`; `--port`, default 7487; `--tailnet ADDR`; `--no-tailnet`; `--volume PATH`; `--compound-sup PATH`,
  default the installed one; `--seats N`, default 32; `--inproc-run DIR`; `--build N` and `--check N`, default 2 and 4; `--tree-root DIR`,
  default the home directory)

## In `<exec_dir>`

`manager.token` (0600), `manager.lock` (the pid; held with `flock` for the manager's life), `journal.jsonl`,
`log/manager.log`, `seats/<seat>/` for the hooks' fold: `burn.json` (the tally `seat.spent` reports) and
`session.json` (the session standing in the seat, which tells a nested session's hooks apart), `run/`
(compound-sup's `COMPOUND_RUN_DIR`: each seat's `<name>.sock`, `<name>.rsock` and `<name>.label`), and `orders/`
(one file per order asked of compound-sup and not yet answered, named by the order id in hex), and `jobs/`: per
job, keyed by `wsp_host::jobs::file_key`, the manager's `<key>.run` (the `job.run`, written before the job
starts) beside the runner's `<key>.job`, `<key>.log` and `<key>.exit`. Nothing prunes them yet (wsp-062).

## Ordering

The journal's mutex is held while an event is appended and sent to the subscribers. It is also held while a new
subscriber's replay is queued. So an event is never sent twice, and none falls between the replay and the live
stream. Locks are always taken in this order: the seat table, then the job table, then hooks, then the journal, then clients, then
`last_beat`. The hooks' fold is held across one post's emits, so a seat's events are journalled in the order its
hooks arrived. The table is never taken while the journal or the hooks' fold is held (`hello` reads its seats
first). The job table is held while a job is forked and its `job.started` journalled, so a retry always finds
the frame it is sent; a stop and the watch run on the job's own thread. No seat process is run under the seat
table's lock: a spawn, a stop, a delivery, `seats` and `seat.read` run on
threads of their own, and answer the asker through its queue.

## Choices beyond the plan

- **Capacity is `--seats`/`--build`/`--check`**, 32/2/4 unless told otherwise. A spawn or a `job.run` past it is
  refused `full`, never queued. wsp-056's install writes the real values.
- **Unrun verbs are answered, not ignored.** `tree.*` gets `refused unsupported`, and a manager-side message
  sent to it gets `refused malformed`.
- **Jobs** (wsp-050). `job.started` and `job.ended` are journalled; `job.output` goes to the job's owner only and
  is not journalled. A row's `tree` is `no_tree` until Q6; wsp names a `dir` under the tree root. A key run
  before, with other arguments or gone from the table, is `duplicate`. Every signal the manager sends is to a
  group in its job table, and `manager.log` names it. A job re-adopted at start has no owner until its key is
  asked again, so the restart does not stop it.
- **What a seat event is.** `seat.started`, `seat.stopped`, `order.delivered`, `order.acked`, `order.refused` and
  `turn` are journalled, sent to every subscriber, and sent once to whoever asked, with their `req` (the asker
  is skipped among the subscribers). `seat.refused`, `refused`, `seats.list` and `seat.screen` are answers only.
- **Spawn refusals before the fork.** An empty argv is `malformed`. `gate: required` is `unsupported`, since local
  seats are not gated (d1 Q12) and the lease is section 5's. A `tree` workdir is `no_tree` until Q6 places
  trees on hosts; wsp sends the seat's cwd. A key whose seat already stopped, a key already running another seat
  or other arguments, and a seat id running under another key are `duplicate`.
- **Env names, values here.** The agent's `env` names are filled from the manager's own environment; a name it
  does not have is left out. `WSP_SEAT_ID` is set to the seat id.
- **The session's pid is the handshake's `sup` line**, not the forked process's pid. They are the same for the
  real compound-sup, which never forks again; when they are, the manager waits on it, and its exit (unasked) is
  reported `seat.stopped` with its status.
- **`delivered` is answered `order.delivered` and `order.acked`**, then a `turn` that began, because
  compound-sup says `delivered` only after the agent's `UserPromptSubmit` hook said the turn began. The ack's
  `turn_began` is when the manager read the answer, until hooks report to the manager (wsp-023). A turn's end
  waits for those hooks too, so `seats` says `working` from an ack until then.
- **`order.cancel`** of an order not yet asked is `order.refused cancelled`, journalled, and the order stays
  cancelled. Once compound-sup has it, the cancel is refused `failed`, because compound-sup owns the delivery.
- **A seat found gone** (its vitals do not answer and its pid is not in the `ps`) at a `seats` request, or at
  start, is reported `seat.stopped` with no exit.
- **The last beat is queued again** just after a client's `hello`. A one-shot CLI call then has the host's facts
  without waiting out a period.
- **The facts are read once per beat**, in three asks of `wsp-host` (`facts::read`: one `sysctl`, one `df`,
  one `pmset`). The beat is shared by every client. A fact the host does not answer is sent at its quiet value
  and logged, because `HostFacts` has no field for "unknown".
- **Not fsynced.** A killed manager keeps what it wrote. A host that loses power is outside the promise.

## Tests

`cargo test -p wsp-manager`. The unit tests cover the journal, the token and the address rule.
`tests/manager.rs` runs a manager on an ephemeral loopback port and checks: a token-less `hello` is closed and a
good one answered; reconnecting `from` N gives exactly the events after N, also across a restart on the same
port; a second manager refuses both the port and the `<exec_dir>`; the beat carries the facts; the last beat
arrives just after `hello`; a client silent for three beats is dropped.

`tests/seats.rs` runs it with a fake compound-sup (a shell script: plain files for sockets, a sleeping process
group for each session, which ends with the test process at the latest) and checks: two spawns with one key
give one seat; a stop or an order naming a stale epoch is refused; an order answers delivered, then acked, and
once only; and the `wsp-manager` binary, killed with SIGKILL mid-order and restarted on its port, still lists
the seat, answers the order once (typed once), and resumes events `from` the last id.

`tests/jobs.rs` runs it with `sh` jobs under a scratch tree root and checks: two `job.run`s under one key run
once; `job.stop` ends the job's group and a process outside it survives; a run past capacity is refused `full`;
a dropped connection stops its undetached job and the detached one runs on; the `wsp-manager` binary, killed
with SIGKILL mid-job and restarted, reports each job's end once and resumes events `from` the last id; and
`jobs` lists each job.
