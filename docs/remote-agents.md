# Remote agents, version 1: an agent works on another machine only while it can talk back

Status: a plan, for Ed to decide. Nothing in it is built. It re-plans wsp-041
(decisions of 2026-08-16) on the stack that exists on 2026-11-05, where compound
has replaced herdr (compound-065, compound-091) and the wsp daemon is a launchd
agent that needs no herdr (wsp-145). Ed's two settled points of 2026-11-05 stand
as given: **v1 is connected-only**, and **the worktree is copied across when the
agent needs it, and its work is copied back** — not pushed git to git, not cloned
on the far side.

Everything below marked *unverified* has not been run; the hand proof (group 3 of the
build, section 9) is what runs it.

## 1. The shape, in one paragraph

A far machine is an **executor**: it runs an agent, in a copy of one row's tree,
under a `compound-sup` that wsp started over ssh. It holds no wsp state; `wsp`
there is a shim that runs the seat's wsp over ssh. The seat keeps the claim, the
seat record, the store and the only authoritative tree. While the link is up the
agent works. When it drops, the agent's next action is held at a gate until the
link is back or a lease runs out. Work returns to the seat by copy, and
`verify`, `review` and `land` only ever read the seat's tree.

What stays from wsp-041, unchanged: executor never a seat; no wsp state on the
far machine; machines reached through `~/.ssh/config` over Tailscale (plain ssh,
not Tailscale SSH); placement asked, never inferred (`spawn --on`); unreachable
is a third state, never empty.

## 2. Audit: what of wsp-044…051, 064 and 067 works on today's stack

The one fact that moves most of this: **compound hosts a supervisor per session,
and each session has its own socket pair in a run directory** (`<name>.sock` for
the durable `compound-sup`, `<name>.rsock` for the disposable `compound-render`,
under `$COMPOUND_RUN_DIR` or `~/.compound/run`). herdr was one server with one
socket. wsp-045's "forward the executor's socket" was therefore one forward per
machine; on compound it is two forwards per seat. And wsp mints the seat id
(`cpd-N`, compound d9) and the machine rides on the seat's record
(`Order::on`, `Census` machine names, robustness-058/059), so the `@machine` id
suffix has nothing left to qualify.

| row | verdict | what it is now |
|---|---|---|
| wsp-044 machines in the store (`machines/<name>.md`, `machines.json`) | **keeps**, two fields change | `herdr_version` becomes `wire` (what the far `compound-sup` answers to `wire-version`, and the far `compound-render`'s). `backend_at` is herdr's socket path; it becomes the far run directory plus the far `compound-sup` path, both written down, not discovered, for the reason the field's own doc gives (no blocking round trip in the daemon's tick). New: `exec_dir`, where trees and per-row state live on the far side (section 6). The doc line "the suffix on a host-qualified id" is stale. |
| wsp-045 tunnel supervisor in the daemon | **keeps the process, changes what it carries** | One ControlMaster per machine stays: backoff curve, ServerAlive, the err log, shutdown-before-reload. The `-L herdr.sock` it holds becomes per-seat forwards added with `ssh -O forward` at spawn and removed at stop. The master dying drops its forwards, so on reconnect the supervisor re-adds them from the seat records, idempotently. The liveness probe stops being `herdr::call_on` and becomes `ssh <m> compound-sup wire-version`, which is also the wire check (section 3). `mirrored_socket()` stays herdr's, as compound-091 says. The unix-socket 104-byte cap (`SUN_PATH_MAX`) applies to every forwarded path: two per seat, short names. |
| wsp-046 host-qualified ids `w0:p3@mb2` | **obsolete for compound** | Seat ids are minted by wsp and carry no host. The machine lives on the seat record and the claim's `host`. No new code routes on a suffix. `herdr::route` stays only for `--herdr --on`, which v1 does not support and need not remove. |
| wsp-047 fan-out of `panes()`/`workspaces()`/`agents()`/`subscribe()` | **replaced by the port's census** | `place.rs` already has `Census::heard(machine, seats)`, `Census::silent(machine, Refusal)` and `and`, which is the fan-out shape wsp-047 built for herdr, written for every backend. A remote machine is one more census entry, silent when the master is down. |
| wsp-048 unreachable is not empty | **keeps, and gets simpler** | The rule is exactly `Census::silent` versus `Census::heard(machine, vec![])`. Today's `may_reap` still parses the machine off an id suffix (`machine_of`); its own doc says it becomes one comparison of two names once the seat record carries the machine. The build must confirm that landed, or land it, before a remote claim can exist (section 4). |
| wsp-049 `spawn --on` | **keeps the flag, replaces the body** | `Order::on` is there. What `--on` does for compound: copy the tree up (section 6), run `compound-sup spawn --label <seat> --cwd <far path> -- <agent>` through ssh (the handshake lines `sup`, `pid`, `socket`, `render` come back on the ssh stdout; `compound-sup` daemonizes with `setsid`, so it outlives the ssh), forward the two sockets, record them on the seat. S in the panel is wsp-053. |
| wsp-050 machines view, wsp-053 S picker | **keep, rework** | Section 7. |
| wsp-051 executor install | **keeps the README shape, changes two steps** | Tailscale, sshd, authorised key, ssh `Host` block, ControlMaster+ControlPersist, the `wsp` shim (exit status and stdin pass-through, both costly to find) all stay. "herdr server under launchd" goes: `compound-sup` is started per seat on demand and daemonizes itself, so nothing long-lived runs on the executor. In its place: the two compound binaries installed at the seat's wire version, a toolchain, and a token for Claude (section 8). The shim stops adding `@$WSP_MACHINE` to `HERDR_PANE_ID`, which no longer exists; it passes `WSP_SEAT_ID` (what `place_super` and `place_compound` already put in the child's environment). |
| wsp-064 what an agent on an executor can see | **still true, one number to re-measure** | The SessionStart brief arrives through the shim; `say` and `note` reach the seat. Per-verb latency was 0.20 s warm; Ed accepted it because only the panel polls and the panel is not there. The gate (section 3) must not add a verb to every tool call, so it reads a local lease file rather than asking the seat. |
| wsp-067 an executor needs the project checked out | **superseded** | The clone from GitHub, mirrored paths and "origin is not where the work is" all go with the copy: the tree arrives from the seat at a path the seat chooses (`<exec_dir>/trees/<row>`), so roots need no host-qualification and wsp-025 is not needed even for a Linux executor. What stays: the executor needs a toolchain that builds the project, for the agent's own iteration. |
| wsp-064/067's finding that `wsp where` names a path that does not exist over there | **kept as a requirement** | `wsp where` and the brief for an executor seat must name the far path, resolved by the claim (`WSP_SEAT_ID`), never by the cwd. *Unverified:* that cwd inference is not on the brief's path today. |

### The wire version across two machines

`place_compound.rs` reads the compound wire by hand (`WIRE_VERSION`, 9 today) and
the supervisor refuses a version it does not know. Across two machines that
becomes a three-way fact: the seat's wsp constant, the far `compound-sup`, the far
`compound-render`. The rule for v1 is **exact match, checked in three places**:

1. every daemon tick, as the liveness probe (`ssh <m> compound-sup wire-version`),
   recorded in `machines.json` as `wire`;
2. at `spawn --on`, which refuses with both numbers and says which binary is
   behind;
3. `wsp doctor`, one line per machine, beside the local `wire_health`.

A seat already running on the far machine keeps the version it started at (it is
`setsid`, so a seat outlives any upgrade). If the seat's wsp moves on, a `tell` to
it is refused as `unsupported ver`, and wsp must show that as **wire mismatch
v9/v10**, not as unreachable: the link is fine. Upgrading an executor means
rebuilding both binaries from the same compound commit *on that machine* (the
architectures may differ); `wsp machine check` (section 7) reports the commit.

## 3. Connected-only, exactly

**Definition.** A remote agent may act only while a heartbeat from the seat is
fresh. "Act" means every tool call: Bash, Edit, Write, and every MCP call. When
the heartbeat is stale the next tool call is held; nothing else is done on the
agent's behalf. The agent is not killed, not told anything it must remember, and
does not know it waited.

**Mechanism (revised 2026-11-06, agent-028).** The beat comes from the seat's
daemon, the lease is short, and the far side is `compound-remote` in compound;
there is no shell loop and no `executor/` script.

- **The beat.** Every 30 s, or a third of the lease where that is shorter
  (`far::beat_every`; `far::BEAT_SECS` is the cap), the daemon sends each far seat on
  each reachable machine `lease <seat> ok` through the seat's forwarded
  controller socket (the `.sock` of section 2), or `lease <seat> stop` once
  nothing holds the seat any more (its claim is released; a seat opened less than
  two beats ago is still told `ok`, since its claim lands a moment after it
  opens). Only seats whose record names the machine are beaten: a local seat
  hears nothing. On the socket a beat is one line,
  `{"ver":1,"lease":{"seat":"<seat>","word":"ok"}}` (`word` is `ok` or `stop`;
  `ver` is compound-remote's own controller wire, `far::REMOTE_WIRE`, not the
  compound-sup session wire). A beat that is answered sets the machine's `heard` time in
  `machines.json`; one that fails changes nothing, so `heard` is the last time
  the far supervisor really answered, which the probe's `last_seen` is not. The
  first beat on a new master is at once, so a held seat is let go as soon as the
  link is back. A machine is silent when `heard` is older than the lease plus one
  beat (a late tick is not a dead link), and a machine with no far seat to beat
  has its `heard` cleared, so it reads off the probe (wsp-588).
- **The lease.** A machine field, in seconds, default 180 (`wsp machine set mb2
  lease=240`): how long a far seat may go unheard before its agent is held. Two to
  three minutes, because a beat every 30 s makes a dead link visible in about one
  beat and the lease is only the allowance for a laptop lid or a flapping route.
  It is no longer 20 minutes, which was a bound on work without a seat, not a
  bound on a missed beat.
- **The gate.** `compound-remote gate --exec-dir <d> --seat <s> --lease <n>`, a
  `PreToolUse` hook (the same pattern as the long-timeout `PermissionHold` hook
  of wsp-301). `spawn --on` writes it into the far tree's `.claude/settings.json`
  before the agent starts, merged into whatever that file holds, with a hook
  timeout of the lease plus 60 s so the hook is not cut off while it waits. The
  gate reads the lease the far `compound-remote` recorded from the last beat
  (local, no network): fresh and `ok` passes at once; stale it waits up to the
  lease and passes if the beat returns, so the agent resumes in the same turn;
  past the lease, or told `stop`, it exits 2 with `seat unreachable beyond the
  lease — stop work and end your turn`, and so does every later call until a
  beat returns. No beat at all is stale: the gate **fails closed**. So that a
  seat's first tool call is not refused before the first beat can reach it (the
  beat goes through a forward made minutes after the spawn, wsp-562), `spawn --on`
  writes a first lease, `ok`, to `<exec_dir>/lease/<seat>` in the same command as
  the gate, before the agent starts. That lease is fresh to the gate for 75 s
  (compound-remote's `lease::FRESH`; wsp passes no `--fresh`): a first call in
  that time passes at once, a later one before the first beat is held, the gate
  polling up to the lease for the beat, and refused only if none comes. Its
  semantics live in the compound-remote row; wsp's half is the beat, the two
  machine fields and the installation.
- **`link`.** A machine field that takes one word, `required`, and refuses
  anything else with a sentence. It is the field version 2 widens (section 4).
- the shim's own exit status. A `wsp` verb that cannot reach the seat exits 75
  (`EX_TEMPFAIL`) with `wsp: the seat is unreachable (link down since <time>)`,
  distinct from a verb that ran and failed, so an agent, a hook and a person can
  each tell a refusal from a partition.

**What it does not stop.** A model call in flight when the link drops finishes
(the far machine talks to Anthropic directly, not through the seat), and a Bash
command already running finishes. The first changes no file; the second may, and
section 6's manifest picks its output up at the next copy-back. v1 stops an
agent's *actions*, not its thinking.

**Why not kill it, and why not wsp-073's ssh-in-a-pane.** A local pane running
`ssh -t <m> claude` dies with the link: the turn is lost, the agent needs
`--resume`, and the seat's pane going away reads as *gone*, which is the
reading wsp-048 exists to forbid. With a durable far supervisor and a gate, a
dropped link and a dead agent are different states in the data.

**How wsp shows it.** A seat on a silent machine has the state **unreachable**,
with the time it was last heard: `unreachable 4m`. It is a fourth word beside
working, idle and gone, drawn in `wsp wip`, the panel row, `wsp brief`'s seat
line and `wsp doctor`, exactly the way a dead daemon is drawn everywhere a seat
looks (wsp-145). The machine reads `unreachable (last seen 4m)` in `machine ls`.
The claim stays held. `reconcile --reap` leaves a claim on a silent machine alone
(`Census::silent`, section 2). `wsp tell` to it is held in the wake spool and
delivered on reconnect rather than dropped (built: `wake::hold_tell` keeps it under `held-tell:<seat>` and the daemon's pass flushes it once the seat reads idle; a seat nothing answers for is refused by `wsp tell` and keeps nothing, and what was held for a seat that is positively gone is dropped; the panel row wears the word without the age): *the spool today drops a level
read for seats that do not exist, so the build must prove it tells an
unreachable seat from a non-existent one.*

**On reconnect.** The master is back within seconds of the link (wsp-561; it
took 5 m 55 s on the i5 before). While a machine is not answering, the daemon
looks at it again every 5 s between its 20 s ticks: a tunnel waiting out its
backoff (which tops out at 15 s) asks a fresh `ssh -o ControlPath=none <m>
true` and dials at once when it answers, and a master that has been `starting`
for 15 s without making its control socket is killed and dialled again, with no
backoff. The probe rides the master or nothing, so a master still connecting is
never probed past by a direct ssh. A slow far side is not a dead link: a
machine that answered in the last 3 min stays reachable (tunnel `slow`)
through failures that reached it, and only a failed connect (`connect to host
…`, no route, refused, unresolved) makes it unreachable at once. Then, in order: re-add the per-seat forwards; probe
the wire version again (a far machine that rebooted may have lost its
supervisors, in which case its census is `heard` and empty, and *that* is when a
claim is reaped, with the rule unchanged); take a census; the seat's state
becomes real again; the daemon's next beat renews the lease; the gate
passes; held tells flush; and a copy-back checkpoint runs (section 6), because
an in-flight command may have changed files. If the seat's wsp was the one that
slept (a laptop lid), the far side sees the same thing and the same lease
applies.

**What is lost if the link never comes back.** Three things, said plainly:

1. Everything the agent did since the last copy-back. The far tree is intact on
   the far machine but nothing on the seat has it. To keep this small, a
   checkpoint copy-back runs at every turn end (`Stop` hook, only the files whose
   hash changed) while connected, so the loss is at most one turn's work.
2. The agent's conversation. Its transcript is on the far machine. A replacement
   agent starts from the brief, the task log and the last checkpoint on the seat.
3. Time: the claim sits held and `unreachable` until a person acts. Nothing is
   released automatically, by design. The person's two verbs, both new:
   `wsp machine retire <m>`, which says the machine is not coming back and lists
   every row held on it, and `wsp despawn <row> --lost`, which releases the
   claim, returns the row to todo, and leaves the last checkpoint in the seat's
   worktree as the starting point. The far machine's copy, if it ever returns, is
   named in the machine's notes and never read automatically.

## 4. What version 1 must not rule out (version 2: disconnected and cloud)

Nothing of version 2 is built. These are the choices v1 has to keep open:

- **Link policy is a field, not a constant.** The gate's wait-or-stop is
  `link: required` on the machine record; v2's "carry on and report later" is
  `link: optional` plus an outbox. v1 has one value and refuses any other.
- **One door for every wsp call from the far side** — the shim — so that v2 can
  queue `say`, `note` and `report` behind it with no change to any agent.
- **Far-side state lives in one directory, `<exec_dir>`**, nowhere else (trees,
  lease, sync base). That is where a v2 outbox would go; it keeps "the executor
  holds no wsp state" true in v1 and gives v2 somewhere to put some.
- **Copy-back is idempotent and keyed by manifest** (section 6), so a copy-back
  of work done during a long outage is the same operation as a checkpoint.
- **The machine record names its transport**, `ssh` today. A cloud agent is a
  machine whose transport is something else; no claim, seat or id may encode "ssh"
  or a Tailscale name.
- **The claim carries machine and lease**, not an id suffix, so a seat that is
  allowed to run unattended later is a longer lease on the same record.

## 5. Settling the parked rows

- **wsp-070 (verify, checkout and land on the seat's tree) — settled.** The shim
  has a short list of *tree verbs*: `verify`, `review`, `checkout`, `land`,
  `commit-help`. For an executor seat the shim does not forward them blindly. It
  first runs a **sync-back** (section 6), refusing on a stale conflict; then runs
  the seat's wsp with the seat-side tree as the subject. `verify` therefore
  builds and tests the copy of the agent's work on the seat, never "the seat's
  checkout with whatever is in it". `checkout` and `land` from an executor are
  refused with a sentence (the row's worktree was made by the seat before the
  spawn, and a verifier lands it, which is already the rule for any row in a run).
  This is the "verbs learn that a tree has a machine" shape, made small because
  the answer to *where* is always the seat.
- **wsp-071 — retire as a duplicate of wsp-070** (its own log says so; the
  refuse-versus-run-locally choice it posed is answered above).
- **wsp-072 (a clone silently behind the seat) — becomes a hash check.** See
  section 6, "Stale".
- **wsp-073 (a remote agent in a local pane) — not in v1.** The need underneath it
  is a person sitting at a remote agent, which v1 does not require, since the
  agent is directed with `tell` and watched through state. When a view is wanted,
  `compound` can attach to a forwarded socket pair, which is the supervisor-per-session
  design doing its job; that is a v1.5 row at most, and *unverified* that attach
  works over a forwarded `.rsock`.
- **wsp-050 / wsp-053 — section 7.**

## 6. The copy

**Unit and place.** One row, one tree. On the seat it is the row's worktree
(`cmd_checkout`'s). On the far machine it is `<exec_dir>/trees/<row>`, an
ordinary directory the seat names in the seat record. The far side's path need not
match anything on the seat.

**When.**

1. **At spawn** (`spawn --on`): copy up, then start the agent. Nothing starts on a
   half copy.
2. **Checkpoint**, on `Stop` while connected: copy back the files whose hash differs
   from the base. Cheap by construction. `spawn --on` writes the hook into the far
   tree's `.claude/settings.json` with the gate: `<exec_dir>/bin/wsp checkpoint`,
   which the seat answers with a files-only copy-back (never a failed turn).
3. **Before `verify`, `review` and `commit-help`**: a full sync-back, through the
   shim's tree verbs (the seat syncs any such verb that arrives from an executor,
   marked by the shim or not). The row's recorded commit is made on the seat from the copied-back
   tree, so a verifier reads the seat's tree and nothing else.
4. On reconnect after a drop, as a checkpoint.

**Mechanism.** `rsync -a` over the machine's existing ControlMaster (`-e "ssh -S
<control>"`), so there is no second authentication. Fallback where rsync is
missing: `tar` over ssh. sftp is acceptable and slower; the choice is a build
detail, not a design one. *Unverified:* macOS's bundled `rsync` (2.6.9-compatible
openrsync on recent releases) does everything needed here; if not, `tar | ssh`.

**What it leaves out.** `target/`, `node_modules/`, `.venv/`, `.worktrees/`, build
output, logs, and `.git`. The reason for `.git`: a worktree's `.git` is a file
pointing at an absolute path in the seat's main repository, which does not exist
over there. Instead the far side gets a plain `git init` with one baseline commit
of the copied tree, and the seat records the seat HEAD it was copied from. The agent
commits on top as the handbook tells it to.

**Build products.** Kept warm on the far side, and outside the tree: the order sets
`CARGO_TARGET_DIR=<exec_dir>/target/<project>` (and the node equivalent), so a
second row of the same project starts with a warm cache and `rsync --delete` never
touches it. The far build is the agent's own iteration loop and **never** the
verdict: the verdict is the seat's `verify` on the copied-back tree. This costs a
cold build on the far side the first time per project, measured by the hand proof.

**How it comes back.** Into the seat's worktree for that row: only files whose far
hash differs from the base, plus deletions, never the excluded set. The agent's
commits come back as `git format-patch` files copied across and applied with
`git am` on the seat, so the log, authors and messages survive. *This is the one
place I stretched Ed's decision: it uses git's patch format as a file format, not
git as a transport. If Ed wants one snapshot commit per copy-back instead, the
messages are lost and the build is simpler.* The recommendation is patches.

**Stale.** A hash check, not a git one. Both sides keep a manifest, one line per
copied file: `mode sha256 path`, sorted, with the manifest's own sha256 as a tree
hash. At copy-up the seat writes the base manifest and records `base_tree`,
`seat_head`, and how far the seat's trunk has moved since (`N commits ahead of
this base`, a number, not silence, which is wsp-072's whole complaint). Three
checks:

1. **Far changed vs base** (what to copy back): far manifest compared with base.
2. **Seat changed since base** (the stale conflict): for every path the agent
   changed, the seat's current hash must equal the base hash. If not, somebody
   changed that file on the seat (a person, a rebase, another row), and the
   copy-back **refuses for those paths**, names them and both hashes, and
   overwrites nothing.
3. **Verify's receipt**: `verify` records the tree hash it ran on, and `review`
   refuses a recorded commit whose tree hash differs. This catches "verified a
   different tree than I am handing over" regardless of which side drifted.

The manifest tool on the far side is a ten-line script beside the shim
(`find … | xargs shasum -a 256`, or `sha256sum` on Linux), not a wsp binary.

## 7. The machines view and choosing a machine (wsp-050, wsp-053)

wsp-050's own log learned the lesson that decides this: a machine read
`connected` through five failed starts, because ssh reachability is not readiness.
So the view has four states, not three: **ready**, **connected but not ready**
(the reason named: wire mismatch, no auth, no toolchain), **unreachable (last seen
…)**, **retired**. `ready` comes from `wsp machine check <m>`: wire matches, the
far `compound-sup` starts and stops a throwaway session, and a one-line headless
`claude -p` answers with the machine's token (section 8) — a check that starts an
agent, which is what the five failures proved the cheaper check does not do. The
daemon runs it on a timer and on `machine add`; the result goes in `machines.json`.

The panel view (wsp-054's) and S's picker (wsp-053) come after the proof, and S
offers only `ready` machines plus this one. `--on` from any shell works before.

## 8. What parked it: the keychain, and what replaces the fix

wsp-041 recorded a three-step fix for eds-macbook-i5: unlock the login keychain
once, clear its idle timeout with `security set-keychain-settings` (no `-t`, no
`-l`), and check that Claude Code had ever logged in there. **That fix still works as a description of macOS and is no longer what
this plan relies on**, for two reasons from wsp-041's own note: it widens what any
process of that user can read, indefinitely, on a machine nobody sits at, and it
makes the executor's availability depend on a setting a laptop's owner changes
without knowing.

Replacement: **a token in the environment, not the keychain.** The executor holds
a long-lived Claude token in a `0600` file under `<exec_dir>`, produced once on
that machine with `claude setup-token` (Ed's manual step). The spawn command exports it into the
`compound-sup` environment on the **far** side
(`ssh <m> 'CLAUDE_CODE_OAUTH_TOKEN=$(cat <file>) compound-sup spawn …'`, with the
expansion happening over there), so the secret never crosses the link, appears
in no seat argv, and is in no wsp log. `claude --bare` stays rejected: it skips
hooks, and the SessionStart hook is how an agent receives its brief
(wsp-041's note). *Unverified, and exactly the first thing the hand proof (group 3) tries, before any code is built on it:*
that Claude Code prefers the environment token over a locked keychain, that it
survives a reboot with nobody logged in, and that the token's own expiry is long
enough to be a setup step and not a weekly chore.

The other two human touches wsp-041 listed, the folder-trust dialog and Claude
Code's setup offer, are once per machine or per folder. Per folder is a problem
when each row has its own tree: the plan has the trees under one parent
(`<exec_dir>/trees`) and the hand proof checks whether trust on the parent covers the
children; if it does not, the spawn command pre-accepts the folder in the far
Claude settings. *Measured (wsp-445): it does not.* Trust on `<exec_dir>/trees` did not
cover `trees/demos-002` on eds-macbook-i5, so the far start now trusts the row's own
tree by name first: `<exec_dir>/bin/wsp-trust add <tree>` (a shell script wsp writes
there, editing `projects["<tree>"].hasTrustDialogAccepted` in the far `.claude.json`
through python3 or node, atomically). `wsp-trust remove <tree>` takes the entry out.
`wsp despawn` of a far seat takes it out again: it copies back what is owed, then
removes the far tree, the far state, the seat's copy record
(`~/.local/state/wsp/trees/<row>`) and the trust entry, so the same row can be
spawned `--on` again. If the copy-back fails, a path was changed on both sides, or
the machine does not answer, nothing is removed and the despawn says what it kept
and why; `despawn --lost` keeps all of it, as it always did.

Which machine: a laptop that sleeps or locks is the wrong shape regardless of
auth, because a sleeping machine is an unreachable one. The manual setup row asks
Ed to name the machine; a machine that stays up is the better choice, and
eds-macbook-i5 with the token is acceptable *if the hand proof passes on it*.

## 9. The build, as groups on the `remote-agents` list

**The order, and why.** The assumptions that parked wsp-018 (auth with nobody
logged in, folder trust, rsync on that machine, the wire match, and whether a
cut link really holds the next tool call) need no new wsp code to test, so
they are tested **first, by hand, on the real machine** (group 3), straight
after Ed's setup. Groups 4 to 6 build on those answers. If one fails, the plan is
corrected before any of them starts: a failed token check changes section 8, a
failed rsync changes the copy row, a gate that does not hold changes section 3.
The gate is prototyped there as a shell loop; the built gate (group 6) is the
same idea with the lease file. The full proof of the built system is group 7.

Every group's effort report (wsp-375) records what a remote row cost against a
local one, including the waits the link adds: copy-up and copy-back bytes and
seconds, cold and warm far build, per-verb ssh latency, every gate wait with its
length, and heartbeat misses. Group 7 runs one local twin of the same row for the
comparison, and the hand proof records the first numbers.

| group | row | kind | what |
|---|---|---|---|
| 2 | agent-024 Ed's setup on one machine | **manual** | Tailscale (check `direct`, not DERP), sshd and authorised key, the `~/.ssh/config` Host block (ControlMaster, ControlPersist, the three timeouts), Claude installed and `setup-token` stored, a toolchain, `compound-sup` and `compound-render` built from the seat's compound commit, `executor/wsp` shim and hooks installed, `wsp machine add`. Done when `ssh <alias> compound-sup wire-version` prints the seat's wire version. |
| 3 | agent-031 Hand proof on the real machine | **manual** | Token with the keychain locked and after a reboot; rsync of a small tree over the ControlMaster; folder trust on a parent; wire match; one agent started by hand with a shell-loop gate, link cut once mid-turn for about 90 s, showing it holds and resumes. First cost numbers. |
| 4 | agent-025 Machine record and readiness | agent | `wire`, `exec_dir`, `wsp machine check`, doctor line, probe on the tick. |
| 4 | agent-026 The copy | agent | manifest, copy-up, copy-back, stale checks, excludes, patches; tested over a local ssh or a stand-in. |
| 5 | agent-027 `spawn --on` through compound-sup over ssh | agent | per-seat forwards, seat record with machine and both sockets, token launch, tree copy-up, `wsp despawn`. |
| 6 | agent-028 Connected-only | agent | heartbeat, gate, shim exit 75 and tree verbs, `unreachable` state in wip/brief/panel/doctor, reap guard on the seat record's machine, wake-spool hold, `machine retire`, `despawn --lost`. |
| 7 | agent-029 The proof of the built system | **manual** | One small row spawned with `wsp spawn --on`; the link cut once inside the lease (it resumes in the same turn), and once past a shortened lease (it stops, then resumes after `tell`). Local twin and the cost table. |
| 8 | agent-030 Machines view and S picker | agent | the panel half of wsp-050/053/054, after the proof. |

The groups run in order, and group 2 is manual, so nothing starts without Ed.

## 10. Open decisions for Ed

1. Patches (`format-patch`/`am`) for the agent's commits, or one snapshot commit
   per copy-back (section 6)? Recommendation: patches.
2. Which machine for the proof (section 8)? Recommendation: one that stays up.
3. The lease: decided 2026-11-06 (agent-028) as 180 s with a 30 s beat (section 3),
   per machine.
4. Retire wsp-071 as the duplicate of wsp-070.

## 11. Heavy rows go to the i5 (agent-040, 2026-11-10)

Status: a plan. Nothing in this section is built. It answers Ed's 2026-11-09
request, made after a night at load 900 and 8.4 GB of swap on the seat: plan how
the run hands work to eds-macbook-i5, and do the first setup by hand. It changes
one rule of section 1: the run now places some rows on a far machine itself, and
the verifier of a far row reads the far tree. Placement is still never guessed.
The project declares what is heavy, and the run only reads that declaration.

**Decided by Ed (2026-11-09).** Only load-heavy rows go to the i5. It is not an
overflow machine, and the rest of the work stays here. The verifier runs on the
i5 in the member's far tree. The barrier runs here, on the trunk, after
copy-back and land. What counts as heavy belongs to each project, not to core
wsp.

### 11.1 "Heavy", declared by the project

A project key, `heavy:`, read the same way as `at_stop:` and `at_review:`:

```
heavy:
  paths:  spikes/web-chrome/
  checks: vitest playwright check:box
  on:     eds-macbook-i5
  setup:  scripts/far-setup.sh
  wait:   10m
```

- **A row is heavy when both matchers match.** `paths` matches a path the row
  names in its overview, its refs or its `check:` lines. `checks` is a word in
  one of its `check:` lines. For compounding that means "files under
  `spikes/web-chrome/` and a box check that runs vitest or Playwright". wsp does
  not read the context pack's *likely files*, because those are inferred and
  placement must not be.
- **`on`** names the machine heavy rows go to. A project with no `heavy:` key has
  no heavy rows. wsp's own project (`agent`) declares none: cargo is warm here,
  and its rows are light. A cargo row on the i5 would start from a cold
  `target/`, so it would cost more than it saves.
- **`setup`** is the project's far-toolchain script (11.4). `wait` is the fallback
  time (11.2).
- **Override, in the list rather than on the row.** Placement is something the
  run decides, so the override lives where the run reads it:
  `wsp worklist member <slug> <task> --place heavy|light --why "…"` and
  `wsp worklist group <slug> N --place heavy|light --why "…"`. The member
  setting beats the group setting, and the group setting beats the
  declaration. A row's own `--heavy` tag is not offered: one row in two lists
  would then have one setting for two runs.
- **Shown.** `worklist show` and `worklist next` print `heavy (spikes/web-chrome/
  + vitest)` or `heavy (member override: …)` beside the member, so a person can
  see why the run placed it there.

### 11.2 Placement in the run

When `worklist advance` spawns a heavy member, it spawns it `--on <heavy.on>`
if the machine is **ready** (section 7's `machine check`, plus the heavy setup
check of 11.4) and has a free place. It runs the same `spawn --on` that a person
would type, so section 1's machinery is unchanged. Only the choice moves.

- **One count per machine.** Each machine has a ceiling: the seat's `agents` (4)
  and the i5's `agents` (2). A seat on the i5 counts against the i5's ceiling,
  not against this Mac's. This changes the cap bullet of wsp-422 ("one count
  across machines"). That bullet also names a per-machine ceiling, so the two
  differ only in whether the fleet has a total as well; that is raised as a
  block on the count row. A list's own `--parallel` still counts all of its
  members on every machine, because it limits work in flight, not load.
- **When the i5 cannot take it.** The member waits up to `heavy.wait` (10 min
  recommended) for the i5 to be ready with a place free. The wait is named as its
  own cause (`waits on eds-macbook-i5: full 2/2` or `… unreachable 3m`). After
  that it **falls back here**, logged on the row as `placed here: <machine>
  <reason> for 10m`. A fallback heavy row counts against this Mac like any
  other. N is an open choice (block).
- **A light row is never placed far.** The i5 is not overflow.

### 11.3 The verifier on the i5

For a far member the run spawns its verifier `--verify --on <the member's
machine>`, in the member's far tree, with no second copy-up:

1. The member's `wsp review` already copies back and records the commit
   (wsp-586). The seat records that commit's tree hash, and the far tree must
   hash the same (section 6, check 3). The verifier refuses to start on a far tree
   that differs, and reads nothing else.
2. The verifier runs the row's checks there, on the i5's toolchain. Its
   `wsp verified` goes back through the shim with the checks' output, the way any
   far verb does (section 3).
3. A held verdict lands on the seat from the seat's copy, which already exists
   because of step 1. **The land reads the seat's tree**, as before. What moves
   is only where the checks ran. Section 5's "verify builds and tests the copy on
   the seat" stays true for light rows and for a person's `wsp verify`.
4. "Checks rerun where the trunk moved" needs a place to run. The rebase happens
   on the seat. The recommendation is to copy the rebased tree up to the same
   far tree and rerun the checks on the i5, since that is the load being moved.
   The alternative is to rerun them here. This is a block on the verifier row.
5. If the i5 goes unreachable during a verify, nothing is lost: the verifier has
   no work of its own, and the seat already has the member's copy. After
   `heavy.wait` the run despawns the far verifier `--lost` and starts a local
   one on the seat's copy.

**The barrier stays here**, on the trunk after copy-back and land, as Ed decided.
A heavy list's barrier still runs its full vitest/Playwright suite on this Mac.
That run is one suite per group instead of one per row, and it is the price of
the trunk being checked where the trunk is.

### 11.4 The far toolchain for heavy rows (the hands-on part)

This part is written as the project's script, `heavy.setup`. For compounding it
is `scripts/far-setup.sh`. It is run first by hand with Ed (group 1) and later by
`wsp machine check` with `--check`:

- **node_modules, one install.** `npm ci` once into
  `<exec_dir>/deps/<project>/web-chrome/node_modules`, keyed by the lockfile's
  hash. At copy-up wsp clones it into the far tree with `cp -c -R` (APFS
  clonefile: no bytes copied, no space used until written). It is never copied up
  from the seat. Section 6 already leaves `node_modules/` out of the copy and out
  of `--delete`. A row whose lockfile differs from the install's hash runs
  `npm ci` in its own far tree, and the cost is logged.
- **Playwright's WebKit.** It is installed once with `npx playwright install
  webkit`, at the revision the lockfile's Playwright pins, in the i5's
  `~/Library/Caches/ms-playwright`.
- **Rust `target/`.** It stays warm per project under `CARGO_TARGET_DIR=<exec_dir>/
  target/<project>` (section 6) if a heavy row builds Rust. The web-chrome box
  checks do not, so this is warmed only if the early proof shows a need.
- **The claude token.** This is section 8's token, already in place for agent-029.
  The check is a headless `claude -p` that answers.
- **Goldens and screenshot baselines (react-goldens).** C7 said the baselines are
  this Mac's WebKit. The i5 runs a different GPU and maybe a different macOS, so
  a pixel compare there may fail on rendering alone. The recommendation, until
  the early proof compares one golden on both machines: a row whose checks compare
  screenshots against goldens is **not heavy**. Its `checks` matcher excludes
  `goldens`, and the golden compare runs here. Whether the i5 gets its own
  baselines is a block.
- **`--check`** prints one line per item: node version, the install's lockfile
  hash against the trunk's, WebKit revision, token, free disk. It exits non-zero
  on any miss. `machine check` runs it over ssh and marks the machine **connected
  but not ready (heavy: <first miss>)**, which is section 7's state with a reason.

### 11.5 A scripted early proof, and a scripted full one

Remote-agents' live proof came last and needed a person at the far machine. It
ran five times over 3.5 days, and each run found a defect that became a fix row
(17 in all); the days between runs were 80 % of that list's wall. The rule for
any plan that touches a far machine (Ed, 2026-11-10): **the live proof is
scripted from the start, and a small one runs early.** The one thing that stays
hands-on is the one-time setup.

- **Setup, by hand (group 1).** 11.4 is done once with Ed and written down as
  `scripts/far-setup.sh --check`. Nothing else in the list needs a person at
  the i5.
- **The early proof (group 2, agent-042).** Right after the setup, before the
  build groups, a script places one heavy row (a components-split group-4
  member, or a react-pass-4 member) with `wsp spawn <row> --on eds-macbook-i5`,
  places its verifier there by `spawn --verify --on` (or runs its checks by
  hand in the far tree, filing what was missing), cuts the link once during the
  verify, and measures:
  - spawn to review, and review to verdict;
  - copy-up and copy-back seconds and bytes, with and without the `cp -c` clone;
  - the i5's load and memory during the row (`vm_stat`, swap, load every 30 s);
  - this Mac's load with and without the row here (a local twin, wsp-422's
    pairing);
  - one golden compared on both machines (11.4).
  A failure is a row and the rerun is one command.
- **The full proof (group 6, agent-047).** The run's placement is turned on for
  one list, and a script runs it, cuts the link once, and reads back the cost
  table against the early proof.

### 11.6 What it must not break

- A far row's work lands only after copy-back **and** a held verifier. The land
  reads the seat's tree, and the tree hash binds the far verdict to it.
- An unreachable i5 **holds** its far seats. It does not lose them (section 3).
  The run does not wait forever on it. New heavy members fall back after
  `heavy.wait`, and a verifier is replaced (11.3, step 5). A held *member* is
  never released automatically, because releasing it loses work. After `heavy.wait`
  the run tells the list's governor, and through it the person, that the member
  is held, with the two verbs (`despawn --lost`, `machine retire`). The list's
  other members go on, and only its barrier waits.
- Light rows, barriers and a person's `spawn --on` behave as they do today.

### 11.7 The build, as the `i5-heavy` list

The list waits after `remote-agents`. Kinds, caps and stops are on the list
itself (`wsp worklist show i5-heavy`).

| group | kind | rows | what |
|---|---|---|---|
| 1 | **manual**, Ed and the primary seat | compounding-337 far setup by hand | 11.4 by hand and written as `scripts/far-setup.sh --check`; the only hands-on group |
| 2 | agent, x1 | agent-042 the scripted early proof | one heavy row placed by script with its verifier there and the link cut once; the measurements of 11.5 |
| 3 | agent, x2 | agent-043 heavy declaration; agent-044 one count per machine | the `heavy:` key, the matchers, the `--place` overrides and the display; per-machine ceilings and waits named for a machine |
| 4 | agent, x1 | agent-045 placement in the run; agent-046 the far verifier (after 045) | advance spawns heavy members `--on` with the fallback, and `machine check` reads the setup check; verifier `--verify --on` in the far tree, the hash bind, the rerun, the verifier fallback, the held-member tell |
| 5 | agent | compounding-338 compounding's declaration | `heavy:` set on compounding, with goldens rows light |
| 6 | agent | agent-047 the scripted full proof | one real list run with placement on by script, link cut once; the cost table against the early proof |

The list is left at `init`: Ed sets it `planned` when the setup can start.

### 11.8 Open choices, raised as blocks

1. The fallback time N (recommendation: 10 min). This blocks agent-045.
2. Whether the fleet has a total cap beside the per-machine ceilings (wsp-422's
   "one count across machines"). Recommendation: no total, only per-machine
   ceilings plus each list's own `--parallel`. This blocks agent-044.
3. Where "checks rerun where the trunk moved" runs for a far member
   (recommendation: the i5). This blocks agent-046.
4. Goldens: whether a goldens row is light (recommendation, until the early proof
   measures it) or the i5 gets its own baselines. This blocks compounding-338.
