# wsp-server

The state core's server (`docs/state-core.md` section 6's `wsp-server` row; state-core step 1, wsp-073), with
only what step 1 serves. That is the read verbs of 4.2 and `subscribe` (4.3), over TCP on loopback and the tailnet
from the start (D19, agent-manager 3.4), on any `Arc<dyn Store + Send + Sync>`, plus the mirror domain. Commands,
leased once-only work (tick, barriers, sweeps, delivery) and the executor half come at steps 2–5. Nothing starts
the server until step 1's group 6 (wsp-075, the daemon).

It depends on `wsp-api` and `wsp-store-contract`, and on `serde_json`. `wsp-store-conformance` (for `MemStore`) is
a dev-dependency. It never links `wsp-store-sqlite`, `wsp-store`, `wsp-wire`, `wsp-host` or `wsp-manager`: section
6 allows the sqlite crate "in `main`" only. `tests/layering.rs` walks its path dependencies to hold this.

## Holds

- `src/lib.rs`: `Server::start(store, Config { addrs, state_dir, port, instance, beat, views })`, `Views` (the
  view verbs' documents, handed in by whatever starts the server: the daemon passes the CLI's own functions,
  wsp-307), `Server::{addrs,
  token, clients, log_path, stop}`, `StartError`. Also `allowed` (the peer and bind rule: `127.0.0.0/8`, `::1`,
  `100.64.0.0/10`, `fd7a:115c:a1e0::/48`, copied from the manager), `ensure_token` (`api.token`: 32 random bytes as
  hex, 0600, narrowed back and logged when widened), `DEFAULT_PORT` (7488, one past the manager's), `LOG_FILE`
  (`api.log`). The listener writes `api.json` whole (a temporary file, then a rename), loopback first. Before it
  binds a fixed port, it dials the port with its own token and refuses to start if anything answers, naming it ("a
  wsp server with this token (instance …, head …)", "a wsp server with another token", "something that is not a
  wsp server"). Accepting is non-blocking with a 25 ms poll, so `stop` needs no self-connect. A peer outside
  `allowed` is closed before it is read.
- `src/conn.rs`: one connection. `hello` comes first: a wrong token is `refused unauthorised`, another protocol
  is `refused protocol`, and any other first frame is `refused malformed`. Each of these closes the connection. A
  good hello gets `welcome { server, protocol, head, by }`, `by` the connection's principal. After that, each request gets one reply by id. A read verb
  is one store call. `lists`, `asks`, `machines`, and `orders` with no seat have no `Reads` verb, so they read the
  one `snapshot`. A command verb is `unsupported`, an unknown verb is `unsupported`, and a known verb with bad
  `args` is `malformed` (a hub's verb, `sync.heads` and the rest, is an unknown verb here). `throttle`, `spend` and `show` are answered by `Config::views` whole (its `Err` as the
  reply's `err`), `unsupported` without one. A request whose `by` is not the connection's is `forbidden`, and one whose `v` is not 1 is
  `protocol`. A line that does not decode is answered `malformed` and the connection stays up. If an `id` can be
  read from the line, the answer is a reply to that id; otherwise it is `refused`. A client silent for
  `MISSED_BEATS` beats is dropped.
- `src/subscribe.rs`: `open` runs before the reply, so `too_old` is the reply. It reads the snapshot (`Feed::State`
  with its id; past `MAX_LINE`, `Feed::Part`s of `SNAPSHOT_PART` bytes and a closing `Feed::State` with the rest,
  wsp-702) or the first page after `from`. `follow` then runs on a thread of its own and writes through the
  connection's shared writer. It follows the head with `wait_for` (at most 200 ms per wait), reads with
  `events_after` from the last id sent, and sends a `Feed::Beat` every beat. It ends with the connection, and
  closes the connection when a write or the store fails. A subscription reads everything: `Feed::Resync` and
  `Feed::Gone` are a hub's scoped frames, which this server never sends.
- `src/tcp.rs`: `Tcp`, the carrier. It is newline JSON over a `TcpStream` and implements `wsp_api::Connection`, with
  `dial`, `recv_line`, `send_line`, `try_clone`, `shutdown` and the 16 MB line cap. It is `pub` for wsp-client's
  tests. It copies `wsp-host`'s carrier, because that one is typed to the manager's `Frame`.
- `src/mirror.rs`: `Mirror`, the `Domain` step 1 runs, plus `subject_of`. `decide` takes `Command::Mirror` alone.
  A `put` entity that differs from the state becomes `mirror.put`, and a `gone` subject the state holds becomes
  `mirror.gone`. A machine that differs only in its beat and its `LIVENESS` facts (`last_seen`, a beat's gauges)
  becomes `machine.beat` instead, payload the bare `Machine` (`beat_only`, wsp-304), so a heartbeat is no entity
  event. Puts are decided first, then gones, each against the drafts before it. Every other verb is
  refused `illegal`, with the state `mirrored`. `evolve` folds both mirror kinds, `machine.beat` as a put, and the
  store's own `lease.*` and `claim.*` events. `text.set` changes nothing. It is pure.
- `tests/server.rs`: the done list, on loopback over `MemStore` (domain `Mirror`). It covers hello and tokens,
  each read verb against the store, commands `unsupported`, malformed lines, `subscribe {}` (the snapshot, then
  every event, no gap, two subscribers in one order), `subscribe {from}` across a restart, `too_old` (a
  `MemStore` wrapped with retention), the silent client dropped while a talking one stays, the start refused and
  named, the mirror's silence on unchanged entities, and 2.3 case 6 run with `Mirror` as the domain.

## Choices beyond the packet

- **The view verbs come from `Views`, not from the store (wsp-307).** Their inputs (this machine's readings, the
  stop ledger, the store's files) are not in a `dyn Store`, and this crate may link neither the CLI nor
  `wsp-view`'s callers, so the daemon hands in an implementation over the very functions `--json` prints from,
  which is what keeps the two byte-identical (`tests/view_golden.rs` holds it). The checks before them are every
  read's: the hello's token and peer, the request's `by` and `v`.

- **`Config` has `port`, `instance` and `beat`** beside `addrs` and `state_dir`. Tests need an ephemeral port and
  a beat of milliseconds. `Config::new` gives `DEFAULT_PORT`, `wsp` and `BEAT_SECS`.
- **A subscriber stays alive by talking.** The API has no client beat frame, and the manager's rule is
  "silent for three beats is dropped", so any frame counts, a malformed one included. A client with nothing to ask
  sends `events { from: <its last>, limit: 0 }` once a beat. wsp-client (wsp-074) does this.
- **A second `subscribe` on one connection is `invalid`**, since a connection holds at most one subscription.
- **The `api.log` file** gets one line per start, refusal, stop, and connection that ends other than by its
  client. The manager's log is the model; the server has no other log to write to until the daemon wires it.
- **No retention in `MemStore`**, so `too_old` is tested through a wrapper store that refuses before an id.
- **Case 6 with `Mirror` is a test of its own.** The suite's `case_snapshot` folds with `TestDomain` and drives
  `row.add`, which `Mirror` refuses, so the test drives mirrors, leases, claims and texts and folds with `Mirror`.

## What it does not serve

The hub is a program of its own repository. The gate a hub's hello went through, the hook its sync verbs were
answered by, TLS, configurable peer ranges, the cutting of a revoked token's connections and scoped subscriptions
were this crate's while the old in-tree hub ran on it, and left with it: the daemon set none of them.
