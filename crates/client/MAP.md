# wsp-client

The state core's client (`docs/state-core.md` section 6's `wsp-client` row; state-core step 1, wsp-074), with only
what step 1 needs: dial and hello, request, and subscribe with reconnect. It is the one way a process outside the
daemon talks to the API: the CLI (wsp-076) and web-chrome (wsp-078, by path dependency). Nothing calls it until
group 7.

It depends on `wsp-api`, `wsp-store-contract` and `serde_json`. `wsp-server` (a loopback server) and
`wsp-store-conformance` (`MemStore`) are dev-dependencies, so a process that only calls does not link the server. It
never links the store, the wire, the host, the manager or the sqlite crate. `tests/layering.rs` walks its path
dependencies to hold this.

## Holds

- `src/lib.rs`: `Client::dial(state_dir, by, wait)`, `Client::{request, request_keyed, set_timeout, server, head,
  subscribe}`, `Error` (`Timeout`, `Lost`, `Silent`, `Args`, `Refused`, `Failed`, `Unexpected`), `REQUEST_TIMEOUT`. `request(verb, args)` builds the
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
  `Item::State`; the parts of a lost connection are dropped.
  The server drops a connection it hears nothing on for `MISSED_BEATS` beats, so once a beat the feed sends the cheap
  read `events { from: <its last>, limit: 0 }` and reads its reply past. `set_beat` tells the feed the server's beat period (`BEAT_SECS` unless a test sets it).

## Tests

`tests/client.rs`, against a loopback `wsp-server` over `MemStore` (ephemeral port, a short beat): `dial` silent
for no `api.json`, a wrong token, no token file, a bad file, a stopped server, and a listener that never welcomes;
`request` for each read verb, and a request after the server was away; `subscribe` for the snapshot then each event
once, a restart mid-stream (`Resumed`, then every later event once, ids contiguous), a feed that never connected
(no `Resumed`), `too_old` (a fresh `State`), and, against a fake server, a repeated id dropped and a silent
connection dialled again; a connection lost before it delivered anything backed off, each wait longer than the
last, and a snapshot in parts handed out as one `State` with the event after it following.

wsp-119's tests run against a scripted server of the test file's own (it welcomes, then asks a closure what to say):
a reply dropped by hanging up, and one never sent, each retried under one key and answered with the first result; a
refusal and a plain `err` sent once; `await_order` for a settled order (no subscribe), a queued one followed to
`acknowledged` past another order's event, `None` past `wait`, and `Silent` within the dial's bound once the server goes.
