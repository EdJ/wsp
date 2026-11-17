# wsp-host-api

The protocol between wsp and the agent manager on each host (`docs/agent-manager.md` section 3, step M0).
A contract crate under `docs/components.md` R2: types, one trait and constants, no IO. Depends on `serde`
alone (`serde_json` only for the `test-support` pair) and on nothing in the workspace, so the manager binary
never pulls in `wsp-store`. Nothing in wsp uses it yet.

## Holds

- `src/lib.rs` — `Frame` (the envelope: journal `id`, request `req`, the message flattened beside its `type`),
  `Message` (every row of 3.2, tagged by its wire name), each message's body, the identities (`RowId`,
  `SeatId`, `HostId`, `OrderId`, `EventId`, `Epoch`, `ReqId`, `At`, `Token`), the fencing keys of 3.3
  (`Key` = `(row, attempt)`, `JobKey`, `Epoch`), the reserved `tree.*` verbs, the refusal reasons
  (`Refusal`, `OrderFail`), and `PROTOCOL`, `BEAT_SECS`, `MISSED_BEATS`
- `src/conn.rs` — `Connection` (send one frame, receive one with a wait) and `ConnError`; the framing rule
  (compact JSON plus one `\n` per frame); `pair`/`Memory`, the in-memory implementation, behind `test-support`
- `src/tests.rs` — the table: one frame of every message, round-tripped, its wire name checked, coverage
  enforced by an exhaustive match; the pinned spelling of `seat.spawn`; older and newer peers decoding

## Contract

Every change is a change to the contract (R5) and is a row of its own. A field added later is
`#[serde(default)]`; a reason added later decodes as `Unknown` on an older peer; a change an older peer
cannot decode bumps `PROTOCOL`.

## Choices beyond the plan's table

- `job.run` is keyed by `JobKey` (row, attempt and the job's name), since one attempt runs several jobs.
- `seats` and `seat.read` are answered by `seats.list` and `seat.screen`, matched by `req`.
- `refused` answers a command with no refusal of its own (`seat.stop`, `job.*`, `tree.*`, a malformed frame).
- `hello` is one message with a `side` (`wsp` or `manager`), since the two carry different facts.
- Cost is in millionths of a dollar and load in hundredths, so every message compares exactly.
