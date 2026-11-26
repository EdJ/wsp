# wsp-client

The state core's client (`docs/state-core.md` section 6's `wsp-client` row; state-core step 1, wsp-074), with only
what step 1 needs: dial and hello, request, and subscribe with reconnect. It is the one way a process outside the
daemon talks to the API: the CLI (wsp-076) and web-chrome (wsp-078, by path dependency). Nothing calls it until
group 7.

It depends on `wsp-api`, `wsp-store-contract`, `wsp-objects` (pure; since wsp-459's sync), `serde_json` and, since wsp-461's TLS, `rustls` (the `ring` provider) and `rustls-native-certs` (the platform's roots). `wsp-server` (a loopback server) and
`wsp-store-conformance` (`MemStore`) are dev-dependencies, so a process that only calls does not link the server. It
never links the store, the wire, the host, the manager or the sqlite crate. `tests/layering.rs` walks its path
dependencies to hold this.

## Holds

- `src/lib.rs`: `Client::dial(state_dir, by, wait)`, `Client::{request, request_keyed, set_timeout, server, head,
  subscribe, wip, throttle, spend, show}`, `Error` (`Timeout`, `Lost`, `Silent`, `Args`, `Refused`, `Failed`, `Unexpected`),
  `ViewError` (`Request`, `Refused`, `Answer`), `REQUEST_TIMEOUT`. `wip()` reads a cut past the line in its parts and absorbs them (wsp-783), starting again on a `stale` part up to `WIP_TRIES` times. `throttle()`, `spend()` and `show(id)` return the
  documents `wsp throttle --json`, `wsp spend --json` and `wsp show <id> --json` print, as the `Value` (wsp-307); a
  refusal (`not_found`, `unsupported` from a server without them) is `ViewError::Refused`. `request(verb, args)` builds the
  envelope from a verb name and JSON `args` (so a verb this build does not know still goes out, and the server
  answers `unsupported`), and returns the `Reply` whose id matches; a refusal is a `Reply` with `err`. One timeout
  covers send to reply; on a timeout or a lost connection the connection is dropped, and the next request dials
  again.
- `src/orders.rs` (wsp-119, step 2's senders): `Client::apply(&Command) -> Applied` mints a key and sends the command
  keyed, again under the same key after a lost reply (`Timeout`) or a lost connection (`Lost`), at most
  `APPLY_ATTEMPTS` sends sharing the one timeout; a server's refusal is `Error::Refused(Refusal)` and any other `err`
  is `Error::Failed(Failure)`, neither retried, and a server that cannot be dialled before anything was sent is
  `Silent` at once. `Client::await_order(id, settled, wait) -> Option<Order>` reads the head, reads `orders` once (an
  order already settled answers at once), then follows `subscribe { from: head }` for `order.*` events on that id
  until one lands it in `settled`; `None` past `wait`; `Error::Silent` once the feed has been unable to connect for
  the dial's `wait`. An `order.refused` is followed by a re-read (it does not say `queued` or `failed`). Window: an
  event between the head read and the `orders` read is replayed, so a refused-and-requeued order can show an older
  state for a moment.
- `src/credential.rs` (wsp-160): `Target` (`Local`, `Hub { url }`) and `Credential` (`Instance`, `User(token)`).
  `Client::dial_hub(url, credential, wait)` dials a hub with the user's token: `tls://host:port` over TLS with the
  platform's roots (wsp-461), `tcp://host:port` or a bare `host:port` in the clear; `Client::dial_hub_ca(.., ca, ..)` pins a
  private CA's PEM file instead (`--ca`, `$WSP_HUB_CA`, `ca` in `hubs.json`: `hub_ca()`), trusting that CA and nothing
  else; a refused certificate is `Silent::Tls`, naming its fault (`src/tls.rs` holds the roots and the name checked, `src/dial.rs` the handshake); `Credential::Instance` is `Silent::NotLoggedIn` and `api.token` is never read for a hub.
  `Credential::for_hub()` is `$WSP_HUB_TOKEN`, else `token` in `~/.config/wsp/hubs.json` (a file wider than 0600 is
  `Silent::BadHubsFile`, naming it; none is `NotLoggedIn`); `hub_url()` is `$WSP_HUB_URL`, else its `url`. The
  `_in` forms take the env value and home as arguments. The hello's `by` is a hint (`$WSP_HUB_USER`, else `$USER`).
  The file is written by `wsp login` (wsp-462, `crates/engine/src/cmd_login.rs`): `{"url", "token", "user", "at"}` and `ca` when pinned; `user` is the
  name `welcome` gave the token (`Client::by()`), which the dial never reads. `hub_login_in(home)` reads the file *without*
  its token (`HubLogin`), for `wsp hub status` and for login saying what it replaces.
- `src/sync.rs` (wsp-459): `Client::sync(workspace, local: &Refs, source: &impl ObjectSource) -> SyncReport`:
  `sync.heads` and `wsp_objects::diff`; fetch every object behind a hub head the spoke lacks, back to one it holds
  (`sync.get`, a round per generation, each held to its id with `verify`); per differing subject push (the spoke's
  head follows the hub's), adopt (the hub's follows the spoke's) or report diverged; one `sync.have` for the objects
  behind the pushed heads and one `sync.put` (parents first) for the missing; one `refs.move` per pushed subject
  from the head it saw. A stale move is `SyncReport::stale` (`Stale { subject, from, head }`), never retried. The
  client stores nothing: `SyncReport { sent, received, refs, moved, adopted, stale, diverged }` is for the spoke's
  store (wsp-464). Parents are read schema-free (one tag around a map's `parent`). `SyncError` (`Request`,
  `Workspace`, `HubWorkspace`, `LocalMissing`, `HubMissing`, `Corrupt`). The verbs one by one: `sync_heads`,
  `sync_have`, `sync_put`, `sync_get` (batched by `IDS_PER_REQUEST` and `PUT_BYTES`) and `refs_move`.
- `src/testing.rs` (behind `test-support`): the in-memory reference hub until wsp-460. `MemHub` answers a `SyncCall`
  (`put` verifies every object before keeping any, writes an id once; `refs.move` from a head that is not there is
  `Move::Stale`), with `refs`, `set_head` and `insert` (another writer, outside the protocol), `holds`, `len`,
  `writes` and `moves`. `FakeHub::start()` serves one on loopback (`FAKE_HUB_TOKEN`; any other verb `unsupported`),
  with `addr`, `token`, `hub`, `shared` and `client(by)`.
- `src/dial.rs`: `Silent` and `open`. `open` reads `api.json` and `api.token`, gives each address an even share of
  what is left of `wait`, and says `hello`. `Silent` is `NoListenFile`, `BadListenFile`, `NoToken`, `Unreachable`
  (each address, with why), `NoWelcome`, `Refused` (the server's code, `unauthorised` for a wrong token) or
  `NotAServer`; its `Display` names which. `Tcp` is the dial-side twin of `wsp-server`'s carrier (newline JSON, a
  16 MB line cap), crate-private.
- `src/subscribe.rs`: `Feed` and `Item`. A feed owns a connection of its own and dials when first read, so
  `subscribe` itself never fails. `next_within(wait)` hands out `Item::State { state, at }`, `Item::Event(e)` and
  `Item::Resumed { from }`; `Iterator::next` blocks until the next one. An event id at or before the last handed
  out is dropped (`duplicates()`). A beat resets the quiet count; each beat period of silence counts a missed beat
  (`missed_beats()`), and `MISSED_BEATS` in a row, or a closed or malformed connection, drops it. It then dials
  again and subscribes `from` its last
  id, and announces that as one `Resumed { from }`, but only when something had been handed out. A `too_old` reply
  leads to `subscribe {}` on the same connection and a fresh `State`, after which events count from its id.
  A loss after the connection delivered an item or a beat dials again at once. A loss before it did (a snapshot
  line it cannot read, say) waits a backoff first, as a failed dial does: 25 ms doubling to 1 s, reset by a
  delivery. `why_silent()` says why it cannot connect (wsp-702). A
  snapshot sent as `Feed::Part`s and a closing `Feed::State` is held until the `State` comes and handed out as one
  `Item::State`; the parts of a lost connection are dropped. On a scoped subscription (wsp-174) ids have gaps, which
  nothing here minds; `Item::Gone { subject, at }` drops a record that left the subscriber's sight, and
  `Item::Resync { state, at }` is a fresh snapshot after a change of scope, folded as the first `State`.
  The server drops a connection it hears nothing on for `MISSED_BEATS` beats, so once a beat the feed sends the cheap
  read `events { from: <its last>, limit: 0 }` and reads its reply past. `set_beat` tells the feed the server's beat period (`BEAT_SECS` unless a test sets it).

## Tests

`tests/client.rs`, against a loopback `wsp-server` over `MemStore` (ephemeral port, a short beat): `dial` silent
for no `api.json`, a wrong token, no token file, a bad file, a stopped server, and a listener that never welcomes;
`request` for each read verb, and a request after the server was away; `subscribe` for the snapshot then each event
once, a restart mid-stream (`Resumed`, then every later event once, ids contiguous), a feed that never connected
(no `Resumed`), `too_old` (a fresh `State`), `throttle`, `spend` and `show` against a server with fake views (and
`unsupported` without them), and, against a fake server, a repeated id dropped and a silent
connection dialled again; a connection lost before it delivered anything backed off, each wait longer than the
last, and a snapshot in parts handed out as one `State` with the event after it following.

`tests/sync.rs` (wsp-459), against `FakeHub`: two spokes with disjoint changes converge in one sync each and a second
sends no object; a thousand records with one changed send (and fetch) one object; `refs.move` from a stale head is
`stale` with the hub's head, and a head moved between `sync.heads` and `refs.move` is reported once, not retried; a
put is held to its hash (a mismatch refuses the whole put) and a second put of one id writes nothing.

wsp-119's tests run against a scripted server of the test file's own (it welcomes, then asks a closure what to say):
a reply dropped by hanging up, and one never sent, each retried under one key and answered with the first result; a
refusal and a plain `err` sent once; `await_order` for a settled order (no subscribe), a queued one followed to
`acknowledged` past another order's event, `None` past `wait`, and `Silent` within the dial's bound once the server goes.
