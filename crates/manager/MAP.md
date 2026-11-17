# wsp-manager

The agent manager (`docs/agent-manager.md`, M1 group 2; wsp-021): one process per host, binary `wsp-manager`.
It depends on `wsp-host-api`, `wsp-base` and `wsp-host`, and on nothing of `wsp-store` or `wsp-wire` (2.3 R1,
d1 Q3). `tests/layering.rs` (`the_manager_links_no_store_and_no_wire`) walks its path dependencies to keep it so.
It runs no seats yet: wsp-022 adds them, and nothing in wsp dials it until wsp-025.

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
- `src/journal.rs`: `<exec_dir>/journal.jsonl`. One frame per line. Ids run from 1 per manager. A torn last line
  is cut at open. `after(from)` serves `subscribe`
- `src/token.rs`: `<exec_dir>/manager.token`, made at first start (32 random bytes as hex, mode 0600) and
  narrowed back to 0600 if it was ever widened
- `src/log.rs`: `<exec_dir>/log/manager.log`, rotated once past 10 MB
- `src/main.rs`: the flags (`--exec-dir`, default `$WSP_MANAGER_DIR` or `~/.local/state/wsp-manager`;
  `--host`; `--port`, default 7487; `--tailnet ADDR`; `--no-tailnet`; `--volume PATH`)

## In `<exec_dir>`

`manager.token` (0600), `manager.lock` (the pid; held with `flock` for the manager's life), `journal.jsonl`,
`log/manager.log`, and `seats/<seat>/` for the hooks' fold: `burn.json` (the tally `seat.spent` reports) and
`session.json` (the session standing in the seat, which tells a nested session's hooks apart).

## Ordering

The journal's mutex is held while an event is appended and sent to the subscribers. It is also held while a new
subscriber's replay is queued. So an event is never sent twice, and none falls between the replay and the live
stream. Locks are always taken in this order: hooks, journal, then clients, then `last_beat`. The hooks' fold
is held across one post's emits, so a seat's events are journalled in the order its hooks arrived.

## Choices beyond the plan

- **Capacity is 0/0/0.** It admits no seats and no jobs yet, and `hello` says so.
- **Seat verbs are answered, not ignored.** `seat.spawn` gets `seat.refused unsupported`, `order.*` gets
  `order.refused no_seat`, `seat.stop` and `seat.read` get `refused no_seat`, `job.*` and `tree.*` get
  `refused unsupported`, and a manager-side message sent to it gets `refused malformed`. `seats` is answered with
  an empty `seats.list`.
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
