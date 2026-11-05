# Remote agents, version 1: an agent works on another machine only while it can talk back

Status: a plan, for Ed to decide. Nothing in it is built. It re-plans wsp-041
(decisions of 2026-08-16) on the stack that exists on 2026-11-05, where compound
has replaced herdr (compound-065, compound-091) and the wsp daemon is a launchd
agent that needs no herdr (wsp-145). Ed's two settled points of 2026-11-05 stand
as given: **v1 is connected-only**, and **the worktree is copied across when the
agent needs it, and its work is copied back** — not pushed git to git, not cloned
on the far side.

Everything below marked *unverified* has not been run; the proof (group 3 of the
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

**Mechanism, three small parts on the far side, none of them wsp state:**

- `executor/heartbeat`, started with the seat by the spawn command and left in its
  process group so it dies with the seat. Every 10 s it runs `wsp lease <seat>`
  through the shim; on success it touches `<exec_dir>/lease/<seat>` and writes the
  seat's answer into it (`ok`, or `stop`, which the seat says when the claim has
  been released). The ssh uses `ConnectTimeout=5`, `ServerAliveInterval=10`,
  `ServerAliveCountMax=3`, so a dead link is noticed in about 30 s and a hung
  call never outlasts the interval.
- `executor/gate`, a `PreToolUse` hook (the same pattern as the long-timeout
  `PermissionHold` hook of wsp-301, so long hook timeouts are already in use
  here). It reads the lease file's age, which is a local `stat` and no network.
  Fresh (under 30 s) and `ok`: exit 0 at once. Stale: it sleeps and re-reads,
  for up to the **lease**, 20 minutes by default, set per machine. Back within
  the lease: exit 0 and the tool call proceeds, so the agent resumes where it was
  in the same turn. Lease out, or the file says `stop`: exit 2 with a plain line,
  `seat unreachable beyond the lease — stop work and end your turn`, and every
  later tool call gets the same until the heartbeat returns. A missing heartbeat
  process makes the file stale, so the gate **fails closed**.
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
delivered on reconnect rather than dropped: *the spool today drops a level
read for seats that do not exist, so the build must prove it tells an
unreachable seat from a non-existent one.*

**On reconnect.** The daemon's backoff tops out at 60 s, so the master is back
within a minute of the link. Then, in order: re-add the per-seat forwards; probe
the wire version again (a far machine that rebooted may have lost its
supervisors, in which case its census is `heard` and empty, and *that* is when a
claim is reaped, with the rule unchanged); take a census; the seat's state
becomes real again; the heartbeat's next beat touches the lease; the gate
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
   from the base. Cheap by construction.
3. **Before `verify` and before `review`**: a full sync-back, through the shim's
   tree verbs. The row's recorded commit is made on the seat from the copied-back
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
cold build on the far side the first time per project, measured by the proof.

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
(wsp-041's note). *Unverified, and exactly the first thing the proof tries:*
that Claude Code prefers the environment token over a locked keychain, that it
survives a reboot with nobody logged in, and that the token's own expiry is long
enough to be a setup step and not a weekly chore.

The other two human touches wsp-041 listed, the folder-trust dialog and Claude
Code's setup offer, are once per machine or per folder. Per folder is a problem
when each row has its own tree: the plan has the trees under one parent
(`<exec_dir>/trees`) and the proof checks whether trust on the parent covers the
children; if it does not, the spawn command pre-accepts the folder in the far
Claude settings. *Unverified.*

Which machine: a laptop that sleeps or locks is the wrong shape regardless of
auth, because a sleeping machine is an unreachable one. The manual setup row asks
Ed to name the machine; a machine that stays up is the better choice, and
eds-macbook-i5 with the token is acceptable *if the proof passes on it*.

## 9. The build, as groups on the `remote-agents` list

Every group's effort report (wsp-375) records what a remote row cost against a
local one, including the waits the link adds: copy-up and copy-back bytes and
seconds, cold and warm far build, per-verb ssh latency, every gate wait with its
length, and heartbeat misses. The proof row (group 6) runs one local twin of the
same row for the comparison.

| group | row | kind | what |
|---|---|---|---|
| 2 | Ed's setup on one machine | **manual** | Tailscale (check `direct`, not DERP), sshd and authorised key, the `~/.ssh/config` Host block (ControlMaster, ControlPersist, the three timeouts), Claude installed and `setup-token` stored, a toolchain, `compound-sup` and `compound-render` built from the seat's compound commit, `executor/wsp` shim and hooks installed, `wsp machine add`. |
| 3 | Machine record and readiness | agent | `wire`, `exec_dir`, `wsp machine check`, doctor line, probe on the tick. |
| 3 | The copy | agent | manifest, copy-up, copy-back, stale checks, excludes, patches; tested over a local ssh or a stand-in, no real machine. |
| 4 | `spawn --on` through compound-sup over ssh | agent | per-seat forwards, seat record with machine and both sockets, token launch, tree copy-up, `wsp despawn`. |
| 5 | Connected-only | agent | heartbeat, gate, shim exit 75 and tree verbs, `unreachable` state in wip/brief/panel/doctor, reap guard on the seat record's machine, wake-spool hold, `machine retire`, `despawn --lost`. |
| 6 | The proof, on the real machine | **manual** | One small row spawned there; the link cut once mid-turn, within the lease (it resumes in the same turn), and once past a shortened lease (it stops, then resumes after `tell`). Checks listed in the row. Includes the local twin and the cost table. |
| 7 | Machines view and S picker | agent | the panel half of wsp-050/053/054, after the proof. |

The groups run in order, and group 2 is manual, so nothing starts without Ed.

## 10. Open decisions for Ed

1. Patches (`format-patch`/`am`) for the agent's commits, or one snapshot commit
   per copy-back (section 6)? Recommendation: patches.
2. Which machine for the proof (section 8)? Recommendation: one that stays up.
3. The lease, 20 minutes by default (section 3): long enough for a laptop sleep,
   short enough that a lost machine does not hold a row all day.
4. Retire wsp-071 as the duplicate of wsp-070.
