# wsp-api

The state core's API on the wire (`docs/state-core.md` section 4; section 6's `wsp-api` row; state-core step 1,
wsp-071). A contract crate under `docs/components.md` R2: types, one trait and constants, no IO. Depends on
`wsp-store-contract` (whose rows, seats, events, commands, refusals and `Feed` it carries), `serde` and
`serde_json`, and on nothing else of the workspace; `tests/layering.rs` holds it to that and to no IO of the standard library.
Will be used by `wsp-server` (wsp-073), `wsp-client` (wsp-074), and through the client by the CLI and web-chrome.

## Holds

- `src/lib.rs` — `Frame` (one line, tagged `type`: `hello`, `welcome`, `refused`, `request`, `reply`, `feed`);
  `Hello` (4.1's credential: `token`, `by`, `client`, `protocol`), `Welcome` (`server`, `protocol`, `head`),
  `Refused` (`code`, `text`); `Request` (4.1's envelope `v`, `id`, `key?`, `by`, then `verb` and `args`) and its
  `Call` (a read `Verb`, a contract `Command`, or `Unknown`); `Verb` (4.2's read verbs, 4.3's `subscribe`, wsp-695's `wip` with wsp-783's `WipArgs { at, part }` and `Answer::Wip`'s `parts`,
  the view verbs `throttle`, `spend` and `show { id }`, wsp-307, and `whoami`, wsp-165: the principal the connection is held as and the instance) and
  `NoArgs`; `Reply` (`id`, `ok` | `err`, `events`), `Outcome`, `Answer` (one per verb), `Failure` and `Code`;
  `ReqId`, `ApiToken`, `Listen`; `PROTOCOL`, `TOKEN_FILE`, `TOKEN_MODE`, `LISTEN_FILE`, `MAX_EVENTS`,
  `READ_VERBS`, `COMMAND_VERBS`; the contract's types re-exported where a frame carries them
- `src/principal.rs` — `Principal`'s text form (`person:ed`, `seat:cpd-3698`, `manager:<host>`,
  `executor:<machine>`, `wsp:<instance>`, `agent:<person>`): `text`, `parse` (a `Malformed` error, never a panic), `KINDS`, and the
  `#[serde(with = "principal")]` helpers the envelope's `by` uses
- `src/conn.rs` — `Connection` (send one frame, receive one with a wait), `ConnError` and `MAX_LINE`; the framing
  rule, copied word for word from `wsp-host-api` (compact JSON plus one `\n`, a 16 MB line cap, `Malformed` keeps the
  connection); `pair`/`Memory`, the in-memory implementation, behind `test-support`
- `tests/round_trip.rs` — tables of every frame type, request verb (each command included), answer, error code and
  `Feed` frame, round-tripped with their wire names, each table's coverage held by an exhaustive match; the verb
  tables held to the variants; `Principal`'s text form for every kind and malformed ones; the pinned spelling;
  peers of other versions; the memory pair's framing

## Contract

Every change is a change to the contract (R5) and is a row of its own. A field added later is
`#[serde(default)]`; a code added later decodes as `Code::Unknown` on an older peer; a verb added later decodes as
`Call::Unknown`; a change an older peer cannot decode bumps `PROTOCOL`. A variant added to the contract's `Command`
or `Principal` fails to compile here (`tests/round_trip.rs`) until `COMMAND_VERBS` or the text form has it.

## Choices beyond section 4

- **Transport (this row's decision (1), as recommended).** Per D19 the API is TCP on loopback and the tailnet from
  the start, not 4.1's unix socket: `$WSP_STATE/api.json` (`Listen { addrs }`) stands for `api.sock`, written by the
  server and read by clients. The credential is one token per instance in `$WSP_STATE/api.token` (0600), carried
  by `hello`, as the manager's is (agent-manager 3.3), in place of 4.1's peer uid. Per-seat tokens are step 6's.
- **Its own `Connection` (this row's decision (2), as recommended).** `wsp-host-api`'s trait is typed to the
  manager's `Frame`; making it generic would change the manager's contract (R5) for no caller's gain, so the trait
  and the memory pair are copied and the two protocols version apart.
- **Every frame carries `type`**, beside 4.1's fields, as host-api's frames do: a request and a feed frame could
  not otherwise be told apart without guessing. A feed frame is the contract's `Feed` in its derived form
  (`{"type":"feed","event":{…}}`, `{"type":"feed","beat":{"beat":N,"at":…}}`), not 4.3's bare `{ "beat": … }`.
- **`hello` comes first**, answered `welcome` or `refused` and then closed; 4.1 has no hello. `refused` carries
  `unauthorised`, `protocol`, or `malformed` (a first frame that is no hello).
- **A connection holds at most one subscription**, so a feed frame names no request. `subscribe` is answered
  `ok: "subscribed"` before its first feed frame, or `err` (`too_old`).
- **`ok` is tagged by its answer** (`{"rows": […]}`, `"applied"`), so a reply decodes without the request beside
  it. A command's `ok` is `applied`; its event ids are the reply's `events`, as 4.1 writes them.
- **`err` is `{code, text}`, and `refusal`** when the code is one of the contract's eight, carried whole (so
  `too_old`'s `oldest` is read, not parsed). `Code` spells those eight as `Refusal`'s `code` tag (a test holds it),
  then the API's own: `malformed`, `unauthorised`, `protocol`, `unsupported` (a command at step 1, or a verb this
  server does not know) and `backend` (the contract's `Error::Backend`).
- **`Call::Unknown`** keeps a request whose verb this build does not read, so the server can answer its `id`:
  `malformed` when the verb is in `READ_VERBS` or `COMMAND_VERBS` (its `args` did not fit), `unsupported` otherwise.
  A verb with no arguments reads `args` left out, `null` or `{}`; any other verb needs its `args`.
- **Arguments 4.2 does not spell**: `orders { seat?, states }` and `asks { kinds, states }` filter, empty is all;
  `events { from, limit? }` returns ids greater than `from`, at most `MAX_EVENTS` (1000), with the `head`.
- **The view verbs (wsp-307)**: `throttle`, `spend` and `show { id }` are answered `Answer::Throttle`, `Spend` and
  `Show`, each the JSON document `wsp throttle --json`, `wsp spend --json` and `wsp show <id> --json` print, as a
  `Value`, so a client printing it pretty has the CLI's bytes; web-chrome reads them in place of spawning the CLI.
  Additive: an older server answers them `unsupported`, so `PROTOCOL` stays 1.
- **The hub's wire is not here.** The sync, id, lease and message verbs, their answers and `fenced` were part of
  this crate while the old in-tree hub ran on `wsp-server`; they are `wsp-hub-protocol`'s (the spoke's), byte for
  byte as they were (its `tests/golden`). A hub verb sent to a local server decodes as `Call::Unknown` and is
  answered `unsupported`.
- **`Principal`'s text form** splits at the first `:`, so a name may hold one; rendering is total, and parsing
  accepts every rendering. `executor:` is the contract's fifth kind, beside the four the row names.
